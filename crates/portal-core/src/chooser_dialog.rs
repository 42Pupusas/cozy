use std::os::fd::RawFd;

use crate::chooser_outcome::ChooserOutcome;
use crate::chooser_request::ChooserRequest;

/// One open file chooser, driven by the daemon's event loop.
pub trait ChooserDialog {
    /// A descriptor that becomes readable when the dialog has work to do.
    fn wake_fd(&self) -> RawFd;

    /// Further descriptors whose readiness means the dialog has work to do,
    /// such as a watch on the folder it shows.
    fn watch_fds(&self) -> Vec<RawFd> {
        Vec::new()
    }

    /// How long the loop may sleep before the dialog next needs to run, or
    /// `None` when it only needs waking by a descriptor.
    fn wait_ms(&self) -> Option<i32> {
        None
    }

    /// Handle pending events without blocking and drain [`Self::wake_fd`].
    /// Returns the outcome once the dialog has ended.
    fn advance(&mut self) -> Option<ChooserOutcome>;

    /// Dismiss the dialog because the request was closed from outside.
    fn close(&mut self);
}

/// Opens a [`ChooserDialog`] for a decoded request.
pub trait DialogFactory {
    /// # Errors
    ///
    /// Returns a message when no dialog can be shown.
    fn open(&mut self, request: ChooserRequest) -> Result<Box<dyn ChooserDialog>, String>;
}

/// A factory for a daemon with no dialog available: every request fails.
pub struct UnavailableDialogs;

impl DialogFactory for UnavailableDialogs {
    fn open(&mut self, _request: ChooserRequest) -> Result<Box<dyn ChooserDialog>, String> {
        Err("no file chooser dialog is available".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chooser_request::{ChooserKind, ChooserRequest};

    fn request() -> ChooserRequest {
        ChooserRequest {
            app_id: String::new(),
            parent_window: String::new(),
            title: String::new(),
            kind: ChooserKind::Open {
                multiple: false,
                directory: false,
            },
            accept_label: None,
            modal: true,
            filters: vec![],
            current_filter: None,
            choices: vec![],
            current_folder: None,
        }
    }

    struct Bare;

    impl ChooserDialog for Bare {
        fn wake_fd(&self) -> RawFd {
            3
        }

        fn advance(&mut self) -> Option<ChooserOutcome> {
            None
        }

        fn close(&mut self) {}
    }

    #[test]
    fn a_dialog_needs_no_extra_descriptors_or_deadline_unless_it_says_so() {
        assert!(Bare.watch_fds().is_empty());
        assert_eq!(Bare.wait_ms(), None);
    }

    #[test]
    fn the_unavailable_factory_always_refuses() {
        let error = UnavailableDialogs.open(request()).err().expect("refusal");
        assert!(error.contains("no file chooser dialog"), "got {error}");
    }
}
