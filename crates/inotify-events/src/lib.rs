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

/// One drained inotify event.
///
/// The name borrows the drain's internal buffer and is only valid inside
/// the callback that received it -- the same contract [`Self::poll_names`]
/// hands its caller, made explicit here as a field because a watcher that
/// watches several directories also needs the `wd` and the raw `mask` to
/// tell the events apart.
pub struct Touched<'a> {
    /// The watch descriptor this event arrived under, as returned by
    /// `Inotify::add_watch`.
    pub wd: i32,
    /// The event mask, kernel-set bits included.
    pub mask: u32,
    /// The entry's name inside the watched directory, when the event
    /// carries one. `None` for events on the watched inode itself
    /// (`DELETE_SELF`, `MOVE_SELF`, and a nameless `ATTRIB`).
    pub name: Option<&'a [u8]>,
}

impl InotifyEvents {
    /// Sized for a handful of coalesced events; `inotify_event` is 16
    /// bytes plus each event's (padded) trailing filename, so this
    /// comfortably covers a burst of saves without needing to loop reads.
    const BUF_LEN: usize = 4096;

    /// Drains every pending event on `ino`, handing each to `on_event` --
    /// the general form of [`Self::poll_names`], which watchers that
    /// register for the nameless events (`DELETE_SELF`, `MOVE_SELF`) need.
    /// Non-blocking: `ino`'s fd is expected to have been opened
    /// `IN_NONBLOCK`. Reads until the kernel queue is empty, because a
    /// directory watch sees bursts far larger than one buffer (copying a
    /// hundred files is a hundred events); a partial read would leave the
    /// rest stranded until some later event arrived.
    pub fn poll_events(ino: &ququmatz::Inotify, mut on_event: impl for<'a> FnMut(Touched<'a>)) {
        let mut buf = [0u8; Self::BUF_LEN];
        loop {
            // EAGAIN (queue empty) or a transient error either way -- both
            // end the drain, not a caller-visible failure. The kernel never
            // splits an event across reads, so parsing per read is safe.
            let Ok(n) = ququmatz::syscall::read(ino.fd(), buf.as_mut_ptr(), buf.len()) else {
                return;
            };
            if n == 0 {
                return;
            }
            let header_size = std::mem::size_of::<InotifyEvent>();
            let mut off = 0usize;
            while off + header_size <= n {
                // SAFETY: `buf[off..]` holds at least `header_size` bytes
                // of a kernel-written `inotify_event`, which is a
                // plain-old-data repr(C) struct with no padding-sensitive
                // invariants -- an unaligned read is exactly what the
                // kernel's own byte-packed event stream requires.
                let ev: InotifyEvent =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off).cast()) };
                let name_len = ev.len as usize;
                let name_start = off + header_size;
                let name_end = name_start + name_len;
                if name_end > n {
                    break; // Truncated trailing event -- stop rather than read garbage.
                }
                let name = if name_len > 0 {
                    // The kernel pads the name with trailing NULs to the
                    // next 4-byte boundary; trim them before handing it to
                    // the caller.
                    let raw = &buf[name_start..name_end];
                    Some(&raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())])
                } else {
                    None
                };
                on_event(Touched {
                    wd: ev.wd,
                    mask: ev.mask,
                    name,
                });
                off = name_end;
            }
            // A short read does NOT mean the queue is empty -- the kernel
            // returns whole-event batches and may stop well before the
            // buffer is full (36 of 100 pending events, measured). Only
            // EAGAIN promises nothing is left, so loop until the read
            // fails.
        }
    }

    /// Drains every pending event on `ino`, calling `on_name` once for
    /// each event that carries a trailing name (i.e. every
    /// `CLOSE_WRITE`/`MOVED_TO` on an entry inside a watched directory;
    /// events on the watched inode itself have no name and are skipped --
    /// use [`Self::poll_events`] when those matter). Non-blocking: `ino`'s
    /// fd is expected to have been opened `IN_NONBLOCK`, so "nothing
    /// pending" (`EAGAIN`) or any other read failure just means "nothing
    /// to report" rather than a stall.
    pub fn poll_names(ino: &ququmatz::Inotify, mut on_name: impl FnMut(&[u8])) {
        Self::poll_events(ino, |ev| {
            if let Some(name) = ev.name {
                on_name(name);
            }
        });
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
        let dir =
            std::env::temp_dir().join(format!("inotify_events_test_{}_a", std::process::id()));
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
        let dir =
            std::env::temp_dir().join(format!("inotify_events_test_{}_b", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(&dir_c, ququmatz::WatchMask::CLOSE_WRITE)
            .expect("add_watch");

        std::fs::write(dir.join("unrelated.txt"), b"noise").unwrap();

        assert!(!InotifyEvents::any_touched(&ino, "target.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The general drain reports the raw mask and the `wd`, and hands
    /// through nameless events on the watched inode itself -- the half
    /// [`Self::poll_names`] filters away, and the half a directory
    /// watcher needs to learn its watch died with the directory.
    #[test]
    fn poll_events_reports_the_wd_mask_and_self_events() {
        let dir =
            std::env::temp_dir().join(format!("inotify_events_test_{}_c", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        let wd = ino
            .add_watch(
                &dir_c,
                ququmatz::WatchMask::CREATE | ququmatz::WatchMask::DELETE_SELF,
            )
            .expect("add_watch");

        std::fs::write(dir.join("target.txt"), b"hello").unwrap();
        let mut events: Vec<(i32, u32, Option<Vec<u8>>)> = Vec::new();
        InotifyEvents::poll_events(&ino, |t| {
            events.push((t.wd, t.mask, t.name.map(<[u8]>::to_vec)));
        });
        assert_eq!(events.len(), 1, "one CREATE with a name");
        assert_eq!(events[0].0, wd, "the wd the watch was registered under");
        assert_eq!(events[0].2.as_deref(), Some("target.txt".as_bytes()));

        // Deleting the watched directory itself reports the nameless
        // DELETE_SELF rather than dropping the event on the floor. The
        // file goes first: a directory can only be removed once empty.
        std::fs::remove_file(dir.join("target.txt")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        let mut self_deleted = false;
        InotifyEvents::poll_events(&ino, |t| {
            self_deleted |= t.name.is_none() && (t.mask & 0x0400) != 0;
        });
        assert!(
            self_deleted,
            "DELETE_SELF must arrive nameless after the directory is removed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A burst of events larger than the parse buffer still reports every
    /// one: the drain must call back once per event, not once per read.
    #[test]
    fn a_burst_bigger_than_the_buffer_reports_every_event() {
        let dir =
            std::env::temp_dir().join(format!("inotify_events_test_{}_d", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(&dir_c, ququmatz::WatchMask::CREATE)
            .expect("add_watch");

        // Names long enough that a handful of events exceed 4096 bytes.
        let long = "z".repeat(80);
        for n in 0..100 {
            std::fs::write(dir.join(format!("{long}{n:04}")), b"x").unwrap();
        }

        let mut seen = 0usize;
        InotifyEvents::poll_events(&ino, |_| seen += 1);
        assert_eq!(
            seen, 100,
            "every event must be reported, including the ones a single read truncated"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
