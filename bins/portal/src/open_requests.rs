//! The file chooser dialogs currently on screen, keyed by request path.

use std::collections::BTreeMap;
use std::os::fd::RawFd;

use dbus_server::{CallContext, NameWatcher};
use portal_core::{ChooserDialog, ChooserOutcome};

struct OpenRequest {
    context: CallContext,
    dialog: Box<dyn ChooserDialog>,
}

/// A dialog that ended and the call it owes an answer to.
pub struct FinishedRequest {
    pub path: String,
    pub context: CallContext,
    pub outcome: ChooserOutcome,
}

#[derive(Default)]
pub struct OpenRequests {
    requests: BTreeMap<String, OpenRequest>,
    owners: NameWatcher,
}

impl OpenRequests {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.requests.contains_key(path)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.requests.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub fn insert(
        &mut self,
        path: impl Into<String>,
        context: CallContext,
        dialog: Box<dyn ChooserDialog>,
    ) {
        let path = path.into();
        self.owners.track(context.sender(), path.clone());
        self.requests.insert(path, OpenRequest { context, dialog });
    }

    #[must_use]
    pub fn wake_fds(&self) -> Vec<RawFd> {
        self.requests
            .values()
            .flat_map(|request| {
                std::iter::once(request.dialog.wake_fd()).chain(request.dialog.watch_fds())
            })
            .collect()
    }

    #[must_use]
    pub fn wait_ms(&self) -> Option<i32> {
        self.requests
            .values()
            .filter_map(|request| request.dialog.wait_ms())
            .min()
    }

    /// Let every dialog handle its pending events and collect the ones that
    /// ended.
    pub fn advance(&mut self) -> Vec<FinishedRequest> {
        let ended: Vec<(String, ChooserOutcome)> = self
            .requests
            .iter_mut()
            .filter_map(|(path, request)| {
                request
                    .dialog
                    .advance()
                    .map(|outcome| (path.clone(), outcome))
            })
            .collect();
        ended
            .into_iter()
            .filter_map(|(path, outcome)| self.finish(&path, outcome))
            .collect()
    }

    /// Dismiss the dialog for `path` because its request was closed.
    pub fn close(&mut self, path: &str) -> Option<FinishedRequest> {
        if let Some(request) = self.requests.get_mut(path) {
            request.dialog.close();
        }
        self.finish(path, ChooserOutcome::Failed)
    }

    /// Dismiss every dialog the vanished connection `owner` opened. The
    /// caller is gone, so nothing is owed an answer.
    pub fn drop_owner(&mut self, owner: &str) -> Vec<String> {
        let paths = self.owners.forget_owner(owner);
        for path in &paths {
            if let Some(mut request) = self.requests.remove(path) {
                request.dialog.close();
            }
        }
        paths
    }

    fn finish(&mut self, path: &str, outcome: ChooserOutcome) -> Option<FinishedRequest> {
        let request = self.requests.remove(path)?;
        self.owners.untrack(path);
        Some(FinishedRequest {
            path: path.to_owned(),
            context: request.context,
            outcome,
        })
    }
}

#[cfg(test)]
pub mod test_support {
    use std::cell::RefCell;
    use std::os::fd::RawFd;
    use std::rc::Rc;

    use portal_core::{ChooserDialog, ChooserOutcome};

    #[derive(Default)]
    pub struct DialogProbe {
        pub closed: bool,
        pub finish_with: Option<ChooserOutcome>,
    }

    pub struct FakeDialog {
        pub fd: RawFd,
        pub probe: Rc<RefCell<DialogProbe>>,
        pub extra_fds: Vec<RawFd>,
        pub deadline: Option<i32>,
    }

    impl FakeDialog {
        #[must_use]
        pub fn boxed(fd: RawFd) -> (Box<dyn ChooserDialog>, Rc<RefCell<DialogProbe>>) {
            let probe = Rc::new(RefCell::new(DialogProbe::default()));
            (
                Box::new(Self {
                    fd,
                    probe: Rc::clone(&probe),
                    extra_fds: Vec::new(),
                    deadline: None,
                }),
                probe,
            )
        }

        #[must_use]
        pub fn waiting(
            fd: RawFd,
            extra_fds: Vec<RawFd>,
            deadline: Option<i32>,
        ) -> Box<dyn ChooserDialog> {
            Box::new(Self {
                fd,
                probe: Rc::new(RefCell::new(DialogProbe::default())),
                extra_fds,
                deadline,
            })
        }
    }

    impl ChooserDialog for FakeDialog {
        fn wake_fd(&self) -> RawFd {
            self.fd
        }

        fn watch_fds(&self) -> Vec<RawFd> {
            self.extra_fds.clone()
        }

        fn wait_ms(&self) -> Option<i32> {
            self.deadline
        }

        fn advance(&mut self) -> Option<ChooserOutcome> {
            self.probe.borrow_mut().finish_with.take()
        }

        fn close(&mut self) {
            self.probe.borrow_mut().closed = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeDialog;
    use super::*;

    fn context(sender: &str, serial: u32) -> CallContext {
        CallContext::new("/org/freedesktop/portal/desktop", sender, serial)
    }

    #[test]
    fn a_dialog_that_has_not_ended_stays_open() {
        let mut open = OpenRequests::new();
        let (dialog, _probe) = FakeDialog::boxed(5);
        open.insert("/r/1", context(":1.1", 1), dialog);
        assert!(open.advance().is_empty());
        assert!(open.contains("/r/1"));
        assert_eq!(open.len(), 1);
    }

    #[test]
    fn an_ended_dialog_is_returned_with_its_call_and_removed() {
        let mut open = OpenRequests::new();
        let (dialog, probe) = FakeDialog::boxed(5);
        open.insert("/r/1", context(":1.1", 7), dialog);
        probe.borrow_mut().finish_with = Some(ChooserOutcome::Cancelled);

        let finished = open.advance();

        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].path, "/r/1");
        assert_eq!(finished[0].context.serial(), 7);
        assert_eq!(finished[0].outcome, ChooserOutcome::Cancelled);
        assert!(open.is_empty());
    }

    #[test]
    fn only_the_ended_dialog_is_removed() {
        let mut open = OpenRequests::new();
        let (first, first_probe) = FakeDialog::boxed(5);
        let (second, _second_probe) = FakeDialog::boxed(6);
        open.insert("/r/1", context(":1.1", 1), first);
        open.insert("/r/2", context(":1.2", 2), second);
        first_probe.borrow_mut().finish_with = Some(ChooserOutcome::Cancelled);

        assert_eq!(open.advance().len(), 1);
        assert!(open.contains("/r/2"));
        assert!(!open.contains("/r/1"));
    }

    #[test]
    fn closing_a_request_closes_its_dialog_and_owes_a_failed_answer() {
        let mut open = OpenRequests::new();
        let (dialog, probe) = FakeDialog::boxed(5);
        open.insert("/r/1", context(":1.1", 3), dialog);

        let finished = open.close("/r/1").expect("was open");

        assert!(probe.borrow().closed);
        assert_eq!(finished.outcome, ChooserOutcome::Failed);
        assert_eq!(finished.context.serial(), 3);
        assert!(open.is_empty());
    }

    #[test]
    fn closing_an_unknown_request_does_nothing() {
        let mut open = OpenRequests::new();
        assert!(open.close("/r/9").is_none());
    }

    #[test]
    fn a_vanished_caller_takes_all_its_dialogs_down_with_no_answer_owed() {
        let mut open = OpenRequests::new();
        let (mine_a, probe_a) = FakeDialog::boxed(5);
        let (mine_b, probe_b) = FakeDialog::boxed(6);
        let (theirs, probe_theirs) = FakeDialog::boxed(7);
        open.insert("/r/1", context(":1.1", 1), mine_a);
        open.insert("/r/2", context(":1.1", 2), mine_b);
        open.insert("/r/3", context(":1.2", 3), theirs);

        let dropped = open.drop_owner(":1.1");

        assert_eq!(dropped, ["/r/1", "/r/2"]);
        assert!(probe_a.borrow().closed && probe_b.borrow().closed);
        assert!(!probe_theirs.borrow().closed);
        assert!(open.contains("/r/3"));
        assert_eq!(open.len(), 1);
    }

    #[test]
    fn a_finished_dialog_is_no_longer_attributed_to_its_caller() {
        let mut open = OpenRequests::new();
        let (dialog, probe) = FakeDialog::boxed(5);
        open.insert("/r/1", context(":1.1", 1), dialog);
        probe.borrow_mut().finish_with = Some(ChooserOutcome::Cancelled);
        let _ = open.advance();

        assert!(open.drop_owner(":1.1").is_empty());
    }

    #[test]
    fn every_open_dialog_contributes_its_wake_fd() {
        let mut open = OpenRequests::new();
        let (first, _a) = FakeDialog::boxed(5);
        let (second, _b) = FakeDialog::boxed(6);
        open.insert("/r/1", context(":1.1", 1), first);
        open.insert("/r/2", context(":1.2", 2), second);
        assert_eq!(open.wake_fds(), [5, 6]);
    }

    #[test]
    fn a_dialogs_extra_descriptors_follow_its_wake_descriptor() {
        let mut open = OpenRequests::new();
        open.insert(
            "/r/1",
            context(":1.1", 1),
            FakeDialog::waiting(5, vec![8, 9], None),
        );
        let (plain, _probe) = FakeDialog::boxed(6);
        open.insert("/r/2", context(":1.2", 2), plain);
        assert_eq!(open.wake_fds(), [5, 8, 9, 6]);
    }

    #[test]
    fn the_loop_sleeps_only_as_long_as_the_soonest_dialog_allows() {
        let mut open = OpenRequests::new();
        assert_eq!(open.wait_ms(), None);
        open.insert(
            "/r/1",
            context(":1.1", 1),
            FakeDialog::waiting(5, vec![], Some(40)),
        );
        assert_eq!(open.wait_ms(), Some(40));
        open.insert(
            "/r/2",
            context(":1.2", 2),
            FakeDialog::waiting(6, vec![], Some(16)),
        );
        let (idle, _probe) = FakeDialog::boxed(7);
        open.insert("/r/3", context(":1.3", 3), idle);
        assert_eq!(open.wait_ms(), Some(16));
    }
}
