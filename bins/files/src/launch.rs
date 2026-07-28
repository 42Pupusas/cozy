//! [`Opener`] -- handing a file to the system's default application.
//!
//! # Why this is four lines and not a desktop-entry resolver
//!
//! Doing this "properly" means parsing `.desktop` files, reading
//! `mimeapps.list`, resolving MIME types, and expanding `Exec=` field codes
//! -- a real subsystem, and one this workspace will eventually want, since
//! `bins/launcher` already has half of it. But a file browser that cannot
//! open a file is not a file browser, and blocking the first working window
//! on a MIME subsystem gets neither done.
//!
//! So: shell out to `xdg-open`, which is present on every desktop this runs
//! on and does exactly the resolution described above. The seam is a named
//! type rather than an inline `Command::spawn` precisely so the real resolver
//! can replace its body later without any call site changing.

use std::path::Path;
use std::process::{Command, Stdio};

/// Opens files with the system default handler.
pub struct Opener;

impl Opener {
    /// Open `path`, detached from this process.
    ///
    /// Failures are reported to stderr and otherwise swallowed: a missing
    /// `xdg-open`, or a handler that refuses to start, must not take the
    /// browser down with it.
    pub fn open(path: &Path) {
        // stdin/stdout/stderr go to null so a handler that decides to print
        // cannot interleave with our own diagnostics or, worse, block on a
        // pipe nobody is draining.
        let spawned = Command::new("xdg-open")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        match spawned {
            // The child is deliberately not waited on. Reaping it would block
            // the event loop for as long as the opened application runs --
            // which for a text editor is "until the user closes it", i.e. a
            // frozen file browser. The cost is a zombie until we exit, which
            // is bounded and harmless for a desktop app's lifetime.
            Ok(_child) => {}
            Err(e) => eprintln!("files: could not open {}: {e}", path.display()),
        }
    }
}
