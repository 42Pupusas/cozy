//! One audited unaligned-read loop over a raw inotify(7) read buffer,
//! shared by every watcher that needs "which files (if any) got touched
//! since I last checked" and nothing more.
//!
//! `bins/terminal`'s `ConfigWatcher` and `bins/portal`'s `ConfigWatcher`
//! used to each hand-roll an identical `unsafe { read_unaligned::<...>()
//! }` loop over the same kernel-defined `inotify_event` byte stream --
//! same buffer size, same padding-trim logic, same safety argument, just
//! copy-pasted. Their *policy* genuinely differs (the terminal applies a
//! reload straight to a live window; the portal only reports that a
//! reload should happen) and deliberately stays un-merged -- see each
//! bin's own `ConfigWatcher` doc comment -- but the mechanical byte
//! parsing underneath has no policy in it at all, so it's pulled out
//! here instead of keeping two `unsafe` blocks that could silently drift
//! apart.

use ququmatz::InotifyEvent;

/// A marker type (not a value with fields) grouping the shared inotify
/// read-buffer parsing under one owner rather than leaving it as a free
/// function.
pub struct InotifyEvents;

impl InotifyEvents {
    /// Sized for a handful of coalesced events; `inotify_event` is 16
    /// bytes plus each event's (padded) trailing filename, so this
    /// comfortably covers a burst of saves without needing to loop reads.
    const BUF_LEN: usize = 4096;

    /// Drains every pending event on `ino`.
    ///
    /// Calls `on_name` once for each event that carries a trailing name
    /// (i.e. every `CLOSE_WRITE`/`MOVED_TO` on an entry inside a watched
    /// directory -- events on the watched inode itself have no name and
    /// are skipped, since none of this workspace's watchers register for
    /// those). Non-blocking: `ino`'s fd is expected to have been opened
    /// `IN_NONBLOCK`, so "nothing pending" (`EAGAIN`) or any other read
    /// failure just means "nothing to report" rather than a stall.
    pub fn poll_names(ino: &ququmatz::Inotify, mut on_name: impl FnMut(&[u8])) {
        let mut buf = [0u8; Self::BUF_LEN];
        // EAGAIN (nothing pending) or a transient error either way -- both
        // mean "report nothing", not a caller-visible failure.
        let Ok(n) = ququmatz::syscall::read(ino.fd(), buf.as_mut_ptr(), buf.len()) else {
            return;
        };
        let header_size = std::mem::size_of::<InotifyEvent>();
        let mut off = 0usize;
        while off + header_size <= n {
            // SAFETY: `buf[off..]` holds at least `header_size` bytes of a
            // kernel-written `inotify_event`, which is a plain-old-data
            // repr(C) struct with no padding-sensitive invariants -- an
            // unaligned read is exactly what the kernel's own byte-packed
            // event stream requires.
            let ev: InotifyEvent = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
            let name_len = ev.len as usize;
            let name_start = off + header_size;
            let name_end = name_start + name_len;
            if name_end > n {
                break; // Truncated trailing event -- stop rather than read garbage.
            }
            if name_len > 0 {
                // The kernel pads the name with trailing NULs to the next
                // 4-byte boundary; trim them before handing it to the
                // caller.
                let raw = &buf[name_start..name_end];
                let trimmed = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
                on_name(trimmed);
            }
            off = name_end;
        }
    }

    /// Convenience for the common case: did any pending event name exactly
    /// `fname`? (Not a prefix or suffix match -- the whole point is
    /// filtering a directory-wide watch down to one specific file.)
    #[must_use]
    pub fn any_touched(ino: &ququmatz::Inotify, fname: &str) -> bool {
        let mut touched = false;
        Self::poll_names(ino, |name| {
            if name == fname.as_bytes() {
                touched = true;
            }
        });
        touched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real inotify watch against a real temp directory: proves the
    /// unaligned-read loop against the kernel's actual byte layout, not
    /// just a hand-built buffer that might not match what `inotify_init1`
    /// really produces.
    #[test]
    fn a_real_write_is_reported_by_name() {
        let dir = std::env::temp_dir().join(format!(
            "inotify_events_test_{}_a",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(&dir_c, ququmatz::WatchMask::CLOSE_WRITE)
            .expect("add_watch");

        // Nothing has happened yet.
        assert!(!InotifyEvents::any_touched(&ino, "target.txt"));

        std::fs::write(dir.join("other.txt"), b"noise").unwrap();
        std::fs::write(dir.join("target.txt"), b"hello").unwrap();

        assert!(InotifyEvents::any_touched(&ino, "target.txt"));
        // A second poll finds nothing new -- the events were drained by
        // the call above, not left pending forever.
        assert!(!InotifyEvents::any_touched(&ino, "target.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A name that never appears must not be reported, even once other
    /// names on the same watch have.
    #[test]
    fn an_untouched_name_is_not_reported() {
        let dir = std::env::temp_dir().join(format!(
            "inotify_events_test_{}_b",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(&dir_c, ququmatz::WatchMask::CLOSE_WRITE)
            .expect("add_watch");

        std::fs::write(dir.join("unrelated.txt"), b"noise").unwrap();

        assert!(!InotifyEvents::any_touched(&ino, "target.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
