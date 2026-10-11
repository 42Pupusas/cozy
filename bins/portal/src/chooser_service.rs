//! `org.freedesktop.impl.portal.FileChooser` and the `Request` objects it
//! exports while a dialog is open.

use dbus_server::{
    Argument, CallContext, Dispatcher, InterfaceDescriptor, MethodDescriptor, PendingCall, Value,
};
use portal_core::ChooserMethod;

use crate::chooser_inbox::ChooserInbox;

pub struct ChooserService;

impl ChooserService {
    pub const IFACE: &'static str = "org.freedesktop.impl.portal.FileChooser";
    pub const REQUEST_IFACE: &'static str = "org.freedesktop.impl.portal.Request";
    pub const REQUEST_PREFIX: &'static str = "/org/freedesktop/portal/desktop/request/";

    const CALL_SIGNATURE: &'static str = "osssa{sv}";
    const REPLY_SIGNATURE: &'static str = "ua{sv}";
    const METHODS: [ChooserMethod; 3] = [
        ChooserMethod::OpenFile,
        ChooserMethod::SaveFile,
        ChooserMethod::SaveFiles,
    ];

    pub fn install(dispatcher: &mut Dispatcher, inbox: &ChooserInbox) {
        dispatcher.register_interface(Self::chooser_descriptor());
        dispatcher.register_interface(Self::request_descriptor());
        for method in Self::METHODS {
            let inbox = inbox.clone();
            dispatcher.register_deferred(
                Self::IFACE,
                method.member(),
                Self::CALL_SIGNATURE,
                Self::REPLY_SIGNATURE,
                move |context: &CallContext, args: &[Value]| {
                    inbox.push_call(
                        method,
                        PendingCall {
                            context: context.clone(),
                            args: args.to_vec(),
                        },
                    );
                    Ok(())
                },
            );
        }
        let inbox = inbox.clone();
        dispatcher.register_method(Self::REQUEST_IFACE, "Close", "", "", move |path, _args| {
            inbox.push_close(path);
            Ok(vec![])
        });
    }

    fn chooser_descriptor() -> InterfaceDescriptor {
        Self::METHODS
            .iter()
            .fold(InterfaceDescriptor::new(Self::IFACE), |descriptor, method| {
                descriptor.method(MethodDescriptor::new(
                    method.member(),
                    vec![
                        Argument::new("handle", "o"),
                        Argument::new("app_id", "s"),
                        Argument::new("parent_window", "s"),
                        Argument::new("title", "s"),
                        Argument::new("options", "a{sv}"),
                    ],
                    vec![
                        Argument::new("response", "u"),
                        Argument::new("results", "a{sv}"),
                    ],
                ))
            })
    }

    fn request_descriptor() -> InterfaceDescriptor {
        InterfaceDescriptor::new(Self::REQUEST_IFACE).method(MethodDescriptor::new(
            "Close",
            vec![],
            vec![],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus_server::CallOutcome;

    const MAIN_PATH: &str = "/org/freedesktop/portal/desktop";
    const REQUEST_PATH: &str = "/org/freedesktop/portal/desktop/request/1_2/t";

    struct Fixture {
        dispatcher: Dispatcher,
        inbox: ChooserInbox,
    }

    impl Fixture {
        fn new() -> Self {
            let inbox = ChooserInbox::new();
            let mut dispatcher = Dispatcher::new();
            dispatcher.register_object(MAIN_PATH, [ChooserService::IFACE]);
            ChooserService::install(&mut dispatcher, &inbox);
            Self { dispatcher, inbox }
        }

        fn call(&self, member: &str) -> Result<CallOutcome, dbus_server::DispatchError> {
            self.dispatcher.dispatch_call(
                &CallContext::new(MAIN_PATH, ":1.5", 8),
                ChooserService::IFACE,
                member,
                "osssa{sv}",
                &[
                    Value::ObjectPath(REQUEST_PATH.into()),
                    Value::Str("app".into()),
                    Value::Str(String::new()),
                    Value::Str("Title".into()),
                    Value::Array(vec![]),
                ],
            )
        }
    }

    #[test]
    fn each_method_defers_and_reaches_the_inbox_with_its_caller() {
        let fixture = Fixture::new();
        for method in ChooserService::METHODS {
            assert_eq!(
                fixture.call(method.member()).expect("accepted"),
                CallOutcome::Deferred
            );
            let (queued, call) = fixture.inbox.pop_call().expect("queued");
            assert_eq!(queued, method);
            assert_eq!(call.context.sender(), ":1.5");
            assert_eq!(call.context.serial(), 8);
            assert_eq!(call.args.len(), 5);
        }
    }

    #[test]
    fn a_wrong_signature_is_rejected_before_it_is_queued() {
        let fixture = Fixture::new();
        let error = fixture
            .dispatcher
            .dispatch_call(
                &CallContext::new(MAIN_PATH, ":1.5", 8),
                ChooserService::IFACE,
                "OpenFile",
                "s",
                &[Value::Str("x".into())],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            dbus_server::DispatchError::InvalidArgs { .. }
        ));
        assert!(fixture.inbox.pop_call().is_none());
    }

    #[test]
    fn close_on_a_request_object_is_queued_and_answered_at_once() {
        let mut fixture = Fixture::new();
        fixture
            .dispatcher
            .register_object(REQUEST_PATH, [ChooserService::REQUEST_IFACE]);
        let outcome = fixture
            .dispatcher
            .dispatch(REQUEST_PATH, ChooserService::REQUEST_IFACE, "Close", "", &[])
            .expect("close");
        assert_eq!(outcome.reply_signature, "");
        assert_eq!(fixture.inbox.pop_close().as_deref(), Some(REQUEST_PATH));
    }

    #[test]
    fn close_on_an_unregistered_request_is_an_unknown_object() {
        let fixture = Fixture::new();
        let error = fixture
            .dispatcher
            .dispatch(REQUEST_PATH, ChooserService::REQUEST_IFACE, "Close", "", &[])
            .unwrap_err();
        assert!(matches!(
            error,
            dbus_server::DispatchError::UnknownObject { .. }
        ));
        assert!(fixture.inbox.pop_close().is_none());
    }

    #[test]
    fn introspection_describes_all_three_methods() {
        let fixture = Fixture::new();
        let outcome = fixture
            .dispatcher
            .dispatch(
                MAIN_PATH,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                "",
                &[],
            )
            .expect("introspect");
        let Value::Str(xml) = &outcome.values[0] else {
            panic!("Introspect must reply with a string");
        };
        for method in ChooserService::METHODS {
            assert!(
                xml.contains(&format!("<method name=\"{}\">", method.member())),
                "missing {} in {xml}",
                method.member()
            );
        }
    }
}
