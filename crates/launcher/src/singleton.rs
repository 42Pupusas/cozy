//! Single-instance enforcement for the launcher.
//!
//! Uses a pathname `AF_UNIX` socket as a mutex + notification channel (no
//! real protocol on top -- any successful connection *is* the "someone
//! else already launched" signal).
//!
//! Why a socket and not a PID/flock file: `bind(2)` on an already-bound
//! path is atomically exclusive (no separate lock-then-check race), and
//! the same primitive doubles as the wakeup channel a second launch uses
//! to tell the first "close yourself" (a launcher hotkey is naturally a
//! toggle -- press once to open, press again to dismiss -- not "open
//! another overlapping window").
//!
//! Pure std (`UnixListener`/`UnixStream`), no libc/Wayland -- headless and
//! unit-tested like the rest of this crate, just pointed at a tempdir
//! socket path instead of `XDG_RUNTIME_DIR` in tests.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::{fs, io};

/// What [`Singleton::acquire`] found.
pub enum SingletonAcquire {
    /// No other instance was running; this process now owns the lock and
    /// should proceed to open its window. Dropping the guard releases the
    /// socket path for the next launch.
    Primary(SingletonGuard),
    /// Another instance is already listening and has been notified (a
    /// connect-then-disconnect, its `listener()`'s accept loop is what a
    /// running launcher watches to know it's time to close) -- this
    /// process should exit immediately without opening a window.
    AlreadyRunning,
}

/// An acquired single-instance lock.
///
/// Holds the bound listener plus the path to unlink on drop. A next launch
/// racing a not-yet-dropped guard (process killed uncleanly, leaving the
/// socket file behind) is handled by [`Singleton::acquire`] itself -- see
/// its doc.
///
/// Deliberately holds no method that produces a [`SingletonAcquire`]: that
/// enum's `Primary` variant holds *this* type, so if acquisition lived here
/// too (`SingletonGuard::acquire() -> SingletonAcquire`) the two types would
/// point at each other -- a real mutual dependency, not just a cargo-graph
/// false positive (unlike the name-collision `Error` one elsewhere in this
/// workspace). [`Singleton`] is the acquisition side of that relationship
/// so this type can stay a plain held-resource with a one-way dependency.
pub struct SingletonGuard {
    listener: UnixListener,
    path: PathBuf,
}

impl SingletonGuard {
    /// The bound listener, for the caller to accept on (each accepted
    /// connection is a "please close" notification from a second launch).
    #[must_use]
    pub const fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

/// Namespace for the single-instance acquisition procedure.
///
/// Not a value in its own right (never constructed) -- just where
/// [`Singleton::acquire`] and its stale-socket retry helper live, kept off
/// [`SingletonGuard`] so that type doesn't depend on [`SingletonAcquire`],
/// which in turn holds a `SingletonGuard`. See `SingletonGuard`'s doc for
/// why that split matters.
pub struct Singleton;

impl Singleton {
    /// Try to become the sole instance bound to `path`.
    ///
    /// * `bind` succeeds → we're first; returns [`SingletonAcquire::Primary`].
    /// * `bind` fails with `AddrInUse` → something else already owns
    ///   `path`. We try to `connect` to it: success means a live instance
    ///   is listening, so we notify it (any bytes at all -- the connection
    ///   itself is the whole signal) and report
    ///   [`SingletonAcquire::AlreadyRunning`]. A refused connect means the
    ///   path is a *stale* socket file left by a process that didn't clean
    ///   up (crash, `SIGKILL`) -- we unlink it and retry the bind once.
    ///
    /// # Errors
    /// Returns the OS error if binding still fails after the stale-socket
    /// retry, or if some other `bind` failure (not `AddrInUse`) occurs.
    pub fn acquire(path: &Path) -> io::Result<SingletonAcquire> {
        match UnixListener::bind(path) {
            Ok(listener) => Ok(SingletonAcquire::Primary(SingletonGuard {
                listener,
                path: path.to_owned(),
            })),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => Self::handle_in_use(path),
            Err(e) => Err(e),
        }
    }

    fn handle_in_use(path: &Path) -> io::Result<SingletonAcquire> {
        if let Ok(mut stream) = UnixStream::connect(path) {
            // The connection itself is the signal; a byte payload just
            // makes `read`-side code on the other end unambiguous (a
            // zero-length read can also mean "peer hung up with no data",
            // which we don't want confused with "no connection at all").
            let _ = stream.write_all(b"toggle");
            Ok(SingletonAcquire::AlreadyRunning)
        } else {
            // Nobody's home -- a crashed prior instance left the socket
            // file behind. Reclaim it and retry the bind once; if that
            // still fails (e.g. permissions), propagate.
            let _ = fs::remove_file(path);
            let listener = UnixListener::bind(path)?;
            Ok(SingletonAcquire::Primary(SingletonGuard {
                listener,
                path: path.to_owned(),
            }))
        }
    }
}

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        // Best-effort: an unlink failure here just leaves a stale path for
        // the next launch's `acquire` to detect and clean up itself.
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique socket path under the system tempdir, so parallel test
    /// runs (and repeated runs) never collide.
    fn temp_socket_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "launcher-singleton-test-{tag}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ))
    }

    #[test]
    fn first_acquire_becomes_primary() {
        let path = temp_socket_path("first-primary");
        let acquired = Singleton::acquire(&path).unwrap();
        assert!(matches!(acquired, SingletonAcquire::Primary(_)));
        // Guard drops at end of scope, unlinking the socket file.
        drop(acquired);
        assert!(!path.exists());
    }

    #[test]
    fn second_acquire_while_primary_is_alive_is_already_running() {
        let path = temp_socket_path("second-already-running");
        let primary = Singleton::acquire(&path).unwrap();
        let SingletonAcquire::Primary(guard) = primary else {
            panic!("expected Primary");
        };

        let second = Singleton::acquire(&path).unwrap();
        assert!(matches!(second, SingletonAcquire::AlreadyRunning));

        // The primary's listener should have a pending connection to
        // accept -- proof the second instance actually notified it rather
        // than silently giving up.
        guard.listener().set_nonblocking(true).unwrap();
        assert!(guard.listener().accept().is_ok());
    }

    #[test]
    fn acquire_after_primary_drops_reclaims_the_path() {
        let path = temp_socket_path("reclaim-after-drop");
        let first = Singleton::acquire(&path).unwrap();
        drop(first);

        let second = Singleton::acquire(&path).unwrap();
        assert!(matches!(second, SingletonAcquire::Primary(_)));
    }

    #[test]
    fn acquire_reclaims_a_stale_socket_left_by_a_dead_process() {
        let path = temp_socket_path("stale-reclaim");
        // Simulate a crashed prior instance: bind a listener and leak it
        // (drop the `UnixListener` without unlinking, exactly what happens
        // when a process is `SIGKILL`ed) -- then nobody is listening, but
        // the path still exists.
        {
            let stale = UnixListener::bind(&path).unwrap();
            drop(stale); // std does NOT unlink on UnixListener::drop
        }
        assert!(path.exists(), "test setup: stale socket should remain");

        let acquired = Singleton::acquire(&path).unwrap();
        assert!(matches!(acquired, SingletonAcquire::Primary(_)));
    }
}
