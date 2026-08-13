//! [`Opener`] -- handing a file to the application that should open it.
//!
//! # Resolution, then fallback
//!
//! The real path is [`Handlers`]: detect the file's type, find the
//! `.desktop` entry associated with it, and expand that entry's `Exec=`
//! line with the path substituted. That is what a file manager is supposed
//! to do, and doing it in-process means the failure modes are visible and
//! testable rather than swallowed by another program.
//!
//! `xdg-open` remains as a fallback for exactly one situation: nothing on
//! this machine claims the file's type. That is a real state -- an obscure
//! extension, a machine with no `mimeinfo.cache`, a stripped container --
//! and `xdg-open` occasionally knows a desktop-environment-specific trick
//! we do not (GNOME's `gio open`, KDE's `kde-open`). Trying it costs one
//! spawn and turns "nothing happened" into "something reasonable happened".
//!
//! The ordering is deliberate and not reversible: consulting `xdg-open`
//! first would make every association we resolve irrelevant.

use crate::{Handlers, Target};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

/// Why an open did not happen.
///
/// A file browser swallows these; a command-line tool must turn them into an
/// exit status, so the reason has to survive as a value rather than as a
/// line already printed to stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Nothing on this machine claims the type or scheme.
    NoHandler,
    /// A handler resolved, but the program would not start.
    SpawnFailed {
        program: String,
        reason: String,
    },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler => write!(f, "no application is associated with this"),
            Self::SpawnFailed { program, reason } => {
                write!(f, "{program} failed to start ({reason})")
            }
        }
    }
}

impl std::error::Error for OpenError {}

/// Opens files with their associated application.
pub struct Opener {
    handlers: Handlers,
}

impl Opener {
    /// Load the system type and association databases.
    ///
    /// Done once at startup rather than per-open: between the glob database
    /// and the association files this reads a fair slice of the filesystem,
    /// and a browser opens files repeatedly over a session.
    #[must_use]
    pub fn load() -> Self {
        Self {
            handlers: Handlers::load(),
        }
    }

    /// Build around an explicit resolver.
    ///
    /// The seam a caller that already holds a [`Handlers`] uses, so a
    /// program that resolves handlers for other reasons -- an "Open With…"
    /// menu, the portal's `OpenURI` -- loads the databases once.
    #[must_use]
    pub const fn new(handlers: Handlers) -> Self {
        Self { handlers }
    }

    /// The resolver behind this opener.
    ///
    /// So a caller that both opens and *reports* -- `bins/open`'s `--list`
    /// and `--dry-run` -- asks the same loaded databases both questions,
    /// rather than paying for a second copy of them.
    #[must_use]
    pub const fn handlers(&self) -> &Handlers {
        &self.handlers
    }

    /// Open `path`, detached from this process.
    ///
    /// Failures are reported to stderr and otherwise swallowed: an
    /// application that refuses to start, or a type nothing handles, must
    /// not take the browser down with it.
    pub fn open(&self, path: &Path) {
        if let Some(command) = self.handlers.command_for(path) {
            match command.spawn_detached() {
                Ok(()) => return,
                // A resolved handler that won't start is worth saying out
                // loud -- it means the association is real but the program
                // is broken or missing from PATH, which is exactly the kind
                // of thing a silent fallback would hide for months.
                Err(e) => eprintln!(
                    "opener: {} failed to start ({e}); falling back",
                    command.program
                ),
            }
        }

        Self::spawn_xdg_open(path.as_os_str());
    }

    /// Open `target`, reporting why if it did not happen.
    ///
    /// The `Result`-returning twin of [`Self::open`], for a caller that has
    /// somewhere to put a failure -- a command-line tool's exit status, a
    /// portal reply. It covers URLs as well as files, and it does **not**
    /// fall back to `xdg-open`: a caller that wants to know the outcome is
    /// poorly served by a spawn whose success says nothing about whether
    /// anything opened.
    pub fn try_open(&self, target: &Target) -> Result<(), OpenError> {
        let command = self
            .handlers
            .command_for_target(target)
            .ok_or(OpenError::NoHandler)?;
        command
            .spawn_detached()
            .map_err(|e| OpenError::SpawnFailed {
                program: command.program.clone(),
                reason: e.to_string(),
            })
    }

    /// [`Self::try_open`], then `xdg-open` if nothing was associated.
    ///
    /// The fallback is worth taking only for [`OpenError::NoHandler`]. A
    /// handler that resolved and then failed to start is a real, specific
    /// fault, and retrying it through another program hides it.
    pub fn try_open_with_fallback(&self, target: &Target) -> Result<(), OpenError> {
        match self.try_open(target) {
            Err(OpenError::NoHandler) => {
                Self::spawn_xdg_open(target.as_argument());
                Ok(())
            }
            other => other,
        }
    }

    /// Last resort: let the desktop environment try.
    fn spawn_xdg_open(argument: &OsStr) {
        // stdin/stdout/stderr go to null so a handler that decides to print
        // cannot interleave with our own diagnostics or, worse, block on a
        // pipe nobody is draining.
        let spawned = Command::new("xdg-open")
            .arg(argument)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        // The child is deliberately not waited on. Reaping it would block
        // the event loop for as long as the opened application runs --
        // which for a text editor is "until the user closes it", i.e. a
        // frozen file browser. The cost is a zombie until we exit, which
        // is bounded and harmless for a desktop app's lifetime.
        if let Err(e) = spawned {
            eprintln!(
                "opener: could not open {}: {e}",
                Path::new(argument).display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mime::{Associations, MimeDb};
    use std::path::PathBuf;

    /// `Opener::open` spawns real processes, so the tests here cover the
    /// decision it makes -- *which* command it would run -- rather than the
    /// spawning itself. The command-building path is shared with
    /// `handlers`, so this is about the wiring being correct, not a second
    /// copy of those rules.
    #[test]
    fn an_opener_resolves_through_its_handlers() {
        let dir = std::env::temp_dir().join(format!("files-opener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("eog.desktop"),
            "[Desktop Entry]\nName=Viewer\nExec=eog %f\nType=Application\n",
        )
        .expect("write");
        let file = dir.join("photo.png");
        std::fs::write(&file, b"x").expect("write");

        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::from_str_data("[Default Applications]\nimage/png=eog.desktop\n"),
            vec![dir.clone()],
        ));

        let cmd = opener.handlers.command_for(&file).expect("resolved");
        assert_eq!(cmd.program, "eog");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A resolvable handler that cannot start must surface as a spawn
    /// failure naming the program -- the case a silent fallback would hide.
    #[test]
    fn a_handler_that_cannot_start_reports_which_program_failed() {
        let dir = std::env::temp_dir().join(format!("opener-spawnfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("broken.desktop"),
            "[Desktop Entry]\nName=Broken\nExec=cozy-nonexistent-program-xyz %f\n\
             Type=Application\n",
        )
        .expect("write");
        let file = dir.join("photo.png");
        std::fs::write(&file, b"x").expect("write");

        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::from_str_data("[Default Applications]\nimage/png=broken.desktop\n"),
            vec![dir.clone()],
        ));

        let error = opener
            .try_open(&Target::File(file))
            .expect_err("the program does not exist");
        assert_eq!(
            error,
            OpenError::SpawnFailed {
                program: "cozy-nonexistent-program-xyz".to_owned(),
                reason: match &error {
                    OpenError::SpawnFailed { reason, .. } => reason.clone(),
                    OpenError::NoHandler => unreachable!(),
                },
            }
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `try_open` must not reach for `xdg-open`, so an unassociated target
    /// is reported rather than silently handed away.
    #[test]
    fn try_open_reports_an_unassociated_target_rather_than_falling_back() {
        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::default(),
            Vec::<PathBuf>::new(),
        ));

        assert_eq!(
            opener.try_open(&Target::parse("ircs://irc.example.com/rust")),
            Err(OpenError::NoHandler)
        );
    }

    #[test]
    fn an_open_error_says_what_went_wrong() {
        assert!(OpenError::NoHandler.to_string().contains("no application"));
        assert_eq!(
            OpenError::SpawnFailed {
                program: "eog".to_owned(),
                reason: "not found".to_owned(),
            }
            .to_string(),
            "eog failed to start (not found)"
        );
    }

    /// An unhandled type must not resolve to a command -- that is what
    /// sends `open` down the `xdg-open` path.
    #[test]
    fn an_unhandled_type_falls_through_to_the_desktop_environment() {
        let opener = Opener::new(Handlers::from_parts(
            MimeDb::from_globs_data("50:image/png:*.png\n"),
            Associations::default(),
            Vec::<PathBuf>::new(),
        ));
        assert!(
            opener
                .handlers
                .command_for(Path::new("/tmp/x.png"))
                .is_none()
        );
    }
}
