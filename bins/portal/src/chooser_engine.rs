//! Turns inbox calls into open dialogs and finished dialogs into replies.

use dbus_server::{CallContext, Dispatcher, PendingCall, ReplySink, Value};
use portal_core::{ChooserMethod, ChooserOutcome, ChooserRequest, DialogFactory};

use crate::chooser_inbox::ChooserInbox;
use crate::chooser_service::ChooserService;
use crate::open_requests::{FinishedRequest, OpenRequests};

const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const REPLY_SIGNATURE: &str = "ua{sv}";

pub struct ChooserEngine {
    inbox: ChooserInbox,
    factory: Box<dyn DialogFactory>,
    open: OpenRequests,
}

impl ChooserEngine {
    #[must_use]
    pub fn new(inbox: ChooserInbox, factory: Box<dyn DialogFactory>) -> Self {
        Self {
            inbox,
            factory,
            open: OpenRequests::new(),
        }
    }

    #[must_use]
    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    #[must_use]
    pub fn wake_fds(&self) -> Vec<std::os::fd::RawFd> {
        self.open.wake_fds()
    }

    #[must_use]
    pub fn wait_ms(&self) -> Option<i32> {
        self.open.wait_ms()
    }

    /// Start dialogs for queued calls, honour queued closes, and answer every
    /// dialog that has ended.
    pub fn pump<S: ReplySink>(&mut self, sink: &mut S, dispatcher: &mut Dispatcher) {
        while let Some(path) = self.inbox.pop_close() {
            if let Some(finished) = self.open.close(&path) {
                Self::answer(finished, sink, dispatcher);
            }
        }
        while let Some((method, call)) = self.inbox.pop_call() {
            self.begin(method, call, sink, dispatcher);
        }
        for finished in self.open.advance() {
            Self::answer(finished, sink, dispatcher);
        }
    }

    /// The bus reported `owner` disconnected.
    pub fn owner_lost(&mut self, owner: &str, dispatcher: &mut Dispatcher) {
        for path in self.open.drop_owner(owner) {
            dispatcher.unregister_object(&path);
        }
    }

    fn begin<S: ReplySink>(
        &mut self,
        method: ChooserMethod,
        call: PendingCall,
        sink: &mut S,
        dispatcher: &mut Dispatcher,
    ) {
        let PendingCall { context, args } = call;
        let request = match ChooserRequest::from_call(method, &args) {
            Ok(request) => request,
            Err(message) => {
                let _ = context.fail(sink, INVALID_ARGS, &message);
                return;
            }
        };
        let Some(Value::ObjectPath(path)) = args.first() else {
            let _ = context.fail(sink, INVALID_ARGS, "handle must be an object path");
            return;
        };
        if dispatcher.has_object(path) {
            let _ = context.fail(
                sink,
                INVALID_ARGS,
                "a request already exists at that handle",
            );
            return;
        }
        match self.factory.open(request) {
            Ok(dialog) => {
                dispatcher.register_object(path.clone(), [ChooserService::REQUEST_IFACE]);
                self.open.insert(path.clone(), context, dialog);
            }
            Err(message) => {
                eprintln!("portal: {}: {message}", method.member());
                Self::send(&context, ChooserOutcome::Failed, sink);
            }
        }
    }

    fn answer<S: ReplySink>(finished: FinishedRequest, sink: &mut S, dispatcher: &mut Dispatcher) {
        dispatcher.unregister_object(&finished.path);
        Self::send(&finished.context, finished.outcome, sink);
    }

    fn send<S: ReplySink>(context: &CallContext, outcome: ChooserOutcome, sink: &mut S) {
        let (code, results) = outcome.into_response();
        let _ = context.reply(sink, &[code.as_value(), results], REPLY_SIGNATURE);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use portal_core::{ChooserDialog, Selection};

    use super::*;
    use crate::open_requests::test_support::{DialogProbe, FakeDialog};

    const MAIN_PATH: &str = "/org/freedesktop/portal/desktop";
    const REQUEST: &str = "/org/freedesktop/portal/desktop/request/1_2/t";

    #[derive(Default)]
    struct RecordingSink {
        returns: Vec<(String, u32, Vec<Value>, String)>,
        errors: Vec<(String, u32, String, String)>,
    }

    impl ReplySink for RecordingSink {
        type Error = std::convert::Infallible;

        fn send_return(
            &mut self,
            destination: &str,
            call_serial: u32,
            values: &[Value],
            reply_signature: &str,
        ) -> Result<(), Self::Error> {
            self.returns.push((
                destination.to_owned(),
                call_serial,
                values.to_vec(),
                reply_signature.to_owned(),
            ));
            Ok(())
        }

        fn send_error(
            &mut self,
            destination: &str,
            call_serial: u32,
            error_name: &str,
            message: &str,
        ) -> Result<(), Self::Error> {
            self.errors.push((
                destination.to_owned(),
                call_serial,
                error_name.to_owned(),
                message.to_owned(),
            ));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FactoryLog {
        requests: Vec<ChooserRequest>,
        probes: Vec<Rc<RefCell<DialogProbe>>>,
    }

    struct FakeFactory {
        log: Rc<RefCell<FactoryLog>>,
        refuse: bool,
    }

    impl DialogFactory for FakeFactory {
        fn open(&mut self, request: ChooserRequest) -> Result<Box<dyn ChooserDialog>, String> {
            if self.refuse {
                return Err("no compositor".to_owned());
            }
            let mut log = self.log.borrow_mut();
            log.requests.push(request);
            let (dialog, probe) = FakeDialog::boxed(10);
            log.probes.push(probe);
            Ok(dialog)
        }
    }

    struct Fixture {
        engine: ChooserEngine,
        dispatcher: Dispatcher,
        inbox: ChooserInbox,
        sink: RecordingSink,
        log: Rc<RefCell<FactoryLog>>,
    }

    impl Fixture {
        fn new(refuse: bool) -> Self {
            let log = Rc::new(RefCell::new(FactoryLog::default()));
            let inbox = ChooserInbox::new();
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_object(MAIN_PATH, [ChooserService::IFACE]);
            ChooserService::install(&mut dispatcher, &inbox);
            let engine = ChooserEngine::new(
                inbox.clone(),
                Box::new(FakeFactory {
                    log: Rc::clone(&log),
                    refuse,
                }),
            );
            Self {
                engine,
                dispatcher,
                inbox,
                sink: RecordingSink::default(),
                log,
            }
        }

        fn args(handle: &str) -> Vec<Value> {
            vec![
                Value::ObjectPath(handle.into()),
                Value::Str("org.example.App".into()),
                Value::Str(String::new()),
                Value::Str("Pick".into()),
                Value::Array(vec![]),
            ]
        }

        fn submit(&mut self, method: ChooserMethod, sender: &str, serial: u32, args: Vec<Value>) {
            self.inbox.push_call(
                method,
                PendingCall {
                    context: CallContext::new(MAIN_PATH, sender, serial),
                    args,
                },
            );
            self.engine.pump(&mut self.sink, &mut self.dispatcher);
        }

        fn pump(&mut self) {
            self.engine.pump(&mut self.sink, &mut self.dispatcher);
        }

        fn probe(&self, index: usize) -> Rc<RefCell<DialogProbe>> {
            Rc::clone(&self.log.borrow().probes[index])
        }
    }

    fn response_code(values: &[Value]) -> u32 {
        let Value::U32(code) = values[0] else {
            panic!("response must be a u32");
        };
        code
    }

    #[test]
    fn a_call_opens_a_dialog_and_exports_the_request_object() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));

        assert_eq!(fixture.engine.open_count(), 1);
        assert!(fixture.dispatcher.has_object(REQUEST));
        assert!(
            fixture.sink.returns.is_empty(),
            "the reply waits for the dialog"
        );
        let log = fixture.log.borrow();
        assert_eq!(log.requests.len(), 1);
        assert_eq!(log.requests[0].app_id, "org.example.App");
    }

    #[test]
    fn a_picked_dialog_replies_to_the_original_call_and_removes_the_object() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));
        fixture.probe(0).borrow_mut().finish_with = Some(ChooserOutcome::Picked(Selection {
            paths: vec![PathBuf::from("/home/me/a.txt")],
            ..Selection::default()
        }));

        fixture.pump();

        assert_eq!(fixture.sink.returns.len(), 1);
        let (destination, serial, values, signature) = &fixture.sink.returns[0];
        assert_eq!(destination, ":1.5");
        assert_eq!(*serial, 8);
        assert_eq!(signature, "ua{sv}");
        assert_eq!(response_code(values), 0);
        let uris = portal_core::VardictReader::new(&values[1]).text_list("uris");
        assert_eq!(uris, ["file:///home/me/a.txt"]);
        assert_eq!(fixture.engine.open_count(), 0);
        assert!(!fixture.dispatcher.has_object(REQUEST));
    }

    #[test]
    fn a_cancelled_dialog_replies_one() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::SaveFile, ":1.5", 9, Fixture::args(REQUEST));
        fixture.probe(0).borrow_mut().finish_with = Some(ChooserOutcome::Cancelled);

        fixture.pump();

        assert_eq!(response_code(&fixture.sink.returns[0].2), 1);
    }

    #[test]
    fn request_close_dismisses_the_dialog_and_answers_two() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));

        fixture.inbox.push_close(REQUEST);
        fixture.pump();

        assert!(fixture.probe(0).borrow().closed);
        assert_eq!(response_code(&fixture.sink.returns[0].2), 2);
        assert!(!fixture.dispatcher.has_object(REQUEST));
    }

    #[test]
    fn a_vanished_caller_closes_its_dialog_and_nothing_is_sent() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));

        fixture.engine.owner_lost(":1.5", &mut fixture.dispatcher);

        assert!(fixture.probe(0).borrow().closed);
        assert!(fixture.sink.returns.is_empty());
        assert!(fixture.sink.errors.is_empty());
        assert!(!fixture.dispatcher.has_object(REQUEST));
        assert_eq!(fixture.engine.open_count(), 0);
    }

    #[test]
    fn a_factory_that_cannot_open_a_dialog_answers_two_at_once() {
        let mut fixture = Fixture::new(true);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));

        assert_eq!(response_code(&fixture.sink.returns[0].2), 2);
        assert_eq!(fixture.engine.open_count(), 0);
        assert!(!fixture.dispatcher.has_object(REQUEST));
    }

    #[test]
    fn malformed_arguments_get_an_invalid_args_error_and_no_dialog() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, vec![Value::U32(1)]);

        assert_eq!(fixture.sink.errors.len(), 1);
        assert_eq!(fixture.sink.errors[0].2, INVALID_ARGS);
        assert_eq!(fixture.engine.open_count(), 0);
    }

    #[test]
    fn a_handle_that_is_not_an_object_path_is_refused() {
        let mut fixture = Fixture::new(false);
        let mut args = Fixture::args(REQUEST);
        args[0] = Value::Str(REQUEST.into());
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, args);

        assert_eq!(fixture.sink.errors.len(), 1);
        assert_eq!(fixture.engine.open_count(), 0);
    }

    #[test]
    fn a_second_call_on_a_live_handle_is_refused_and_the_first_dialog_survives() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));
        fixture.submit(ChooserMethod::OpenFile, ":1.6", 9, Fixture::args(REQUEST));

        assert_eq!(fixture.sink.errors.len(), 1);
        assert_eq!(fixture.sink.errors[0].1, 9);
        assert_eq!(fixture.engine.open_count(), 1);
        assert!(!fixture.probe(0).borrow().closed);
    }

    #[test]
    fn two_callers_can_have_dialogs_open_together() {
        let mut fixture = Fixture::new(false);
        fixture.submit(ChooserMethod::OpenFile, ":1.5", 8, Fixture::args(REQUEST));
        fixture.submit(
            ChooserMethod::OpenFile,
            ":1.6",
            9,
            Fixture::args("/org/freedesktop/portal/desktop/request/1_3/t"),
        );

        assert_eq!(fixture.engine.open_count(), 2);
        assert_eq!(fixture.engine.wake_fds(), [10, 10]);
    }

    #[test]
    fn a_close_for_a_request_that_already_ended_is_ignored() {
        let mut fixture = Fixture::new(false);
        fixture.inbox.push_close(REQUEST);
        fixture.pump();
        assert!(fixture.sink.returns.is_empty());
    }
}
