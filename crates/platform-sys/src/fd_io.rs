//! The write-bytes-then-close response shape shared by Wayland `send` replies.
//!
//! Every `wl_data_source`/`zwlr_data_control_source` `send` response takes
//! the same shape: the compositor (or a peer client) hands us a pipe fd
//! out-of-band, we owe it our payload followed by EOF, and closing the fd
//! *is* EOF.
//!
//! Pulled out because the `IntoRawFd` → `File::from_raw_fd` → `write_all` →
//! drop-closes sequence was hand-duplicated three times across the crate
//! graph (`platform::clipboard`, `platform::toplevel::drag`,
//! `clipboard_wayland::clipboard_session`) with the same `SAFETY` reasoning
//! copy-pasted each time — exactly the kind of repetition that drifts out of
//! sync the next time one call site's comment gets tweaked and the other two
//! don't.

use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

/// Extension trait on [`OwnedFd`] for the write-then-close response shape.
pub trait OwnedFdExt {
    /// Consume this fd, write `bytes` into it as a plain file, then let it
    /// close on scope exit (signalling EOF to whatever is reading the other
    /// end). Best-effort: a write failure is silently dropped, matching the
    /// original call sites — there is no good recovery for a peer that
    /// stopped reading its side of the pipe.
    fn write_all_and_close(self, bytes: &[u8]);
}

impl OwnedFdExt for OwnedFd {
    fn write_all_and_close(self, bytes: &[u8]) {
        use std::io::Write as _;
        let raw = self.into_raw_fd();
        // SAFETY: `self` was an `OwnedFd` — a file descriptor we exclusively
        // owned — so reconstituting it as a `File` and letting that `File`
        // drop (closing the fd) is exactly the well-formed handoff
        // `IntoRawFd`/`FromRawFd` exist for; nothing else observes `raw`
        // between the two calls.
        let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
        let _ = file.write_all(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::OwnedFdExt;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    /// Build a pipe as an `(OwnedFd read-end, OwnedFd write-end)` pair for
    /// tests, without pulling in a whole libc dependency for `pipe(2)` --
    /// this crate already declares its own raw externs elsewhere, so one
    /// more test-only extern is consistent with that pattern.
    fn pipe_pair() -> (OwnedFd, OwnedFd) {
        unsafe extern "C" {
            fn pipe(fds: *mut i32) -> i32;
        }
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe(2)` to fill.
        let rc = unsafe { pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe(2) failed");
        // SAFETY: `pipe` on success wrote two fresh, valid, owned fds.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    #[test]
    fn write_all_and_close_delivers_bytes_then_eof() {
        let (read_end, write_end) = pipe_pair();
        write_end.write_all_and_close(b"hello");

        // SAFETY: `read_end` is a live, owned fd for the whole test.
        let mut reader = unsafe { std::fs::File::from_raw_fd(read_end.as_raw_fd()) };
        std::mem::forget(read_end); // reader now owns the fd exclusively
        let mut got = Vec::new();
        reader.read_to_end(&mut got).expect("read side of the pipe");
        assert_eq!(got, b"hello");
    }
}
