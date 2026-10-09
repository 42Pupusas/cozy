//! [`FileLauncher`] -- opening files once their activation token is known.
//!
//! The token request is answered by the compositor some time after the click
//! that asked for it, so an open is held against its ticket and performed
//! when the answer arrives.

use std::path::PathBuf;

use platform::toplevel::activation_report::ActivationReport;
use platform::toplevel::{PendingLaunches, Toplevel};

/// Opens files with the user's associated handler, carrying an activation
/// token when the compositor issued one.
pub struct FileLauncher {
    opener: opener::Opener,
    report: ActivationReport,
    pending: PendingLaunches<PathBuf>,
}

impl FileLauncher {
    /// Load the association databases once.
    ///
    /// They are a few thousand lines between them, and re-reading disk on
    /// every Enter would put that latency directly in the user's way.
    pub fn load() -> Self {
        Self {
            opener: opener::Opener::load(),
            report: ActivationReport::from_env("files"),
            pending: PendingLaunches::new(),
        }
    }

    /// Request a token for `path` and hold the open until it is answered.
    ///
    /// The token is requested per open, because it is spent by the launch it
    /// authorises: it carries the serial of the click that asked for this
    /// file, so the compositor lets the handler's window come to the user
    /// rather than treating it as focus stealing.
    pub fn request(&mut self, window: &mut Toplevel, path: PathBuf) {
        let ticket = window.request_activation_token(None);
        self.pending.hold(ticket, path);
    }

    /// Open every file whose token request has been answered.
    pub fn open_answered(&mut self, window: &mut Toplevel) {
        while let Some(outcome) = window.next_activation() {
            let Some(path) = self.pending.take(outcome.ticket) else {
                continue;
            };
            self.report
                .record(outcome.token.as_ref(), window.supports_activation());
            self.opener.set_activation(
                outcome
                    .token
                    .map(|token| opener::ActivationToken::new(token.into_string())),
            );
            self.opener.open(&path);
        }
    }
}
