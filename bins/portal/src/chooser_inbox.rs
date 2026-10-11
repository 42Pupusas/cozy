//! Calls the dispatcher accepted for the file chooser, waiting for the loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dbus_server::PendingCall;
use portal_core::ChooserMethod;

#[derive(Default)]
struct InboxState {
    calls: VecDeque<(ChooserMethod, PendingCall)>,
    closes: VecDeque<String>,
}

/// A shared hand-off between handlers inside the dispatcher and the loop
/// that owns the connection and the dialogs.
#[derive(Clone, Default)]
pub struct ChooserInbox {
    state: Arc<Mutex<InboxState>>,
}

impl ChooserInbox {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_call(&self, method: ChooserMethod, call: PendingCall) {
        self.lock().calls.push_back((method, call));
    }

    #[must_use]
    pub fn pop_call(&self) -> Option<(ChooserMethod, PendingCall)> {
        self.lock().calls.pop_front()
    }

    pub fn push_close(&self, request_path: &str) {
        self.lock().closes.push_back(request_path.to_owned());
    }

    #[must_use]
    pub fn pop_close(&self) -> Option<String> {
        self.lock().closes.pop_front()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus_server::CallContext;

    fn call(serial: u32) -> PendingCall {
        PendingCall {
            context: CallContext::new("/p", ":1.1", serial),
            args: vec![],
        }
    }

    #[test]
    fn calls_keep_their_method_and_arrival_order() {
        let inbox = ChooserInbox::new();
        inbox.push_call(ChooserMethod::SaveFile, call(1));
        inbox.push_call(ChooserMethod::OpenFile, call(2));
        let (first, first_call) = inbox.pop_call().expect("first");
        assert_eq!(first, ChooserMethod::SaveFile);
        assert_eq!(first_call.context.serial(), 1);
        assert_eq!(inbox.pop_call().expect("second").0, ChooserMethod::OpenFile);
        assert!(inbox.pop_call().is_none());
    }

    #[test]
    fn closes_are_a_separate_ordered_list() {
        let inbox = ChooserInbox::new();
        inbox.push_close("/a");
        inbox.push_close("/b");
        assert!(inbox.pop_call().is_none());
        assert_eq!(inbox.pop_close().as_deref(), Some("/a"));
        assert_eq!(inbox.pop_close().as_deref(), Some("/b"));
        assert_eq!(inbox.pop_close(), None);
    }

    #[test]
    fn clones_share_one_state() {
        let inbox = ChooserInbox::new();
        let producer = inbox.clone();
        producer.push_close("/x");
        assert_eq!(inbox.pop_close().as_deref(), Some("/x"));
    }
}
