use std::os::fd::RawFd;

use crate::chooser_outcome::ChooserOutcome;
use crate::chooser_request::ChooserRequest;

/// One open file chooser, driven by the daemon's event loop.
pub trait ChooserDialog {
    /// A descriptor that becomes readable when the dialog has work to do.
    fn wake_fd(&self) -> RawFd;

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

    #[test]
    fn the_unavailable_factory_always_refuses() {
        let error = UnavailableDialogs.open(request()).err().expect("refusal");
        assert!(error.contains("no file chooser dialog"), "got {error}");
    }
}
