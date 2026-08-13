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

use crate::Handlers;
use std::path::Path;
use std::process::{Command, Stdio};

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

        Self::spawn_xdg_open(path);
    }

    /// Last resort: let the desktop environment try.
    fn spawn_xdg_open(path: &Path) {
        // stdin/stdout/stderr go to null so a handler that decides to print
        // cannot interleave with our own diagnostics or, worse, block on a
        // pipe nobody is draining.
        let spawned = Command::new("xdg-open")
            .arg(path)
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
            eprintln!("opener: could not open {}: {e}", path.display());
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
