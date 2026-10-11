//! [`DirsWatch`] -- inotify eyes on the directory the browser has open.
//!
//! `files` used to read a directory exactly when the user navigated: a file
//! another process created, renamed, moved in or deleted stayed invisible
//! until the user left and came back. This watcher closes that gap with one
//! kernel watch on the open directory, drained from the event loop's own
//! pass -- the same best-effort shape `bins/terminal` and `bins/portal` use
//! for config saves, with the shared [`InotifyEvents`] parser underneath.
//!
//! Deliberately narrow: the watcher reports only that *something* in the
//! open directory changed. What to do about it (re-read, keep the cursor on
//! the same file, re-rank the tiles) is the browser's business, exactly as
//! the manual F5 path already does it.

use std::ffi::CString;
use std::path::Path;

use inotify_events::InotifyEvents;

/// The inotify instance, when the kernel gave us one, and the single watch
/// currently armed on it.
///
/// One watch at a time: the browser shows one directory, and every watch
/// left behind on a directory the user navigated away from would keep
/// firing refreshes for directories nobody is looking at.
pub struct DirsWatch {
    ino: Option<ququmatz::Inotify>,
    /// The watch descriptor currently armed, as `add_watch` returned it.
    /// `None` once the kernel has reaped it or nothing was ever armed.
    armed: Option<i32>,
}

impl Default for DirsWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl DirsWatch {
    /// Events that mean "the listing or an entry in it may have changed".
    ///
    /// Creation, deletion and both halves of a rename cover the listing
    /// itself; `CLOSE_WRITE` catches a file whose contents were saved in
    /// place (mtime and size move, which the tiles rank by); `ATTRIB`
    /// catches `touch` and `chmod`, which leave the bytes alone but the
    /// metadata worth re-showing. The watched directory's own
    /// `DELETE_SELF`/`MOVE_SELF` are included so a directory removed out
    /// from under the browser is reported instead of watched at a ghost.
    ///
    /// What is *not* subscribed is what the browser's own re-read
    /// produces -- `OPEN`, `ACCESS`, `CLOSE_NOWRITE` on the directory
    /// itself -- which is what keeps a refresh from re-triggering itself.
    fn watch_mask() -> ququmatz::WatchMask {
        ququmatz::WatchMask::CREATE
            | ququmatz::WatchMask::DELETE
            | ququmatz::WatchMask::MOVED_FROM
            | ququmatz::WatchMask::MOVED_TO
            | ququmatz::WatchMask::CLOSE_WRITE
            | ququmatz::WatchMask::ATTRIB
            | ququmatz::WatchMask::DELETE_SELF
            | ququmatz::WatchMask::MOVE_SELF
    }

    /// The kernel's receipt for a watch it removed -- including the one
    /// this module removes when re-arming elsewhere. It arrives in the
    /// shared queue with the dead watch's descriptor, reports nothing
    /// about any directory, and must not ask for a refresh.
    /// (`IN_IGNORED` is 0x8000; the nearby 0x4000 `IN_Q_OVERFLOW` is
    /// deliberately *not* filtered -- a lost event is a change.)
    const IGNORED: u32 = 0x0000_8000;

    /// Set up the inotify instance, best-effort: a kernel without inotify
    /// (or out of instances) leaves the watcher permanently quiet and the
    /// browser falls back to navigation-time reads, exactly as before.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ino: ququmatz::Inotify::new().ok(),
            armed: None,
        }
    }

    /// Watch `dir` -- the directory the browser just (re)read.
    ///
    /// Re-arms from scratch on every successful read rather than skipping
    /// the already-open directory: if the directory was deleted and
    /// recreated while unwatched, the new inode is a different watch
    /// target, and only a fresh `add_watch` follows it again.
    pub fn follow(&mut self, dir: &Path) {
        let Some(ino) = &self.ino else {
            return;
        };
        if let Some(wd) = self.armed.take() {
            // A stale watch descriptor (the kernel removed it with the
            // deleted directory) just fails here; that is fine.
            let _ = ino.remove_watch(wd);
        }
        let Ok(target) = CString::new(dir.as_os_str().as_encoded_bytes()) else {
            return; // interior NUL cannot name a real directory
        };
        match ino.add_watch(&target, Self::watch_mask()) {
            Ok(wd) => self.armed = Some(wd),
            Err(error) => {
                self.armed = None;
                eprintln!("files: watching {}: {error}", dir.display());
            }
        }
    }

    /// The watched inotify fd, for the event loop's `poll(2)`.
    ///
    /// `None` while nothing is armed, so a loop that would otherwise block
    /// on this fd keeps its idle-timeout behaviour instead of waiting on a
    /// descriptor that can never fire.
    #[must_use]
    pub fn fd(&self) -> Option<i32> {
        let ino = self.ino.as_ref()?;
        self.armed.as_ref().map(|_| ino.fd().as_i32())
    }

    /// Drain every pending event, reporting whether any of them asks for a
    /// re-read.
    ///
    /// A burst of events (a copy landing, a save) collapses into one
    /// `true`: the caller refreshes once and that re-read sees the whole
    /// burst. Queue overflow is a change too -- it means events were
    /// *lost*, and a re-read is the only honest response.
    ///
    /// The kernel's receipt for a removed watch (`IN_IGNORED`) is not a
    /// change; when it retires the armed watch -- the open directory was
    /// deleted out from under us -- the arm is dropped, so [`Self::fd`]
    /// stops reporting a descriptor that can never carry a real event
    /// again and the event loop falls back to its idle timeout.
    pub fn poll(&mut self) -> bool {
        let Some(ino) = self.ino.as_ref() else {
            return false;
        };
        let Some(armed_wd) = self.armed else {
            return false;
        };
        let mut changed = false;
        let mut reaped = false;
        InotifyEvents::poll_events(ino, |touched| {
            if touched.mask & Self::IGNORED != 0 {
                reaped |= touched.wd == armed_wd;
                return;
            }
            changed = true;
        });
        if reaped {
            self.armed = None;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::DirsWatch;
    use crate::test_support::TempDir;

    /// A file created after the watch was armed is reported exactly once:
    /// the drain must not re-report what it already handed over.
    #[test]
    fn a_created_file_is_reported_once() {
        let dir = TempDir::new("watch_create");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());
        assert!(watch.fd().is_some(), "an armed watch has an fd to poll");

        dir.create_file("new.txt", b"x");
        assert!(watch.poll(), "creation must be reported");
        assert!(!watch.poll(), "and must not be reported twice");
    }

    /// Deletion, rename and a save-in-place each ask for a re-read -- the
    /// three changes the old navigate-only model could never show.
    #[test]
    fn deletions_renames_and_saves_are_reported() {
        let dir = TempDir::new("watch_kinds");
        let file = dir.create_file("note.txt", b"v1");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());

        std::fs::rename(&file, dir.path().join("moved.txt")).expect("rename");
        assert!(watch.poll(), "a rename is a create plus a delete");

        std::fs::remove_file(dir.path().join("moved.txt")).expect("remove");
        assert!(watch.poll(), "a deletion must be reported");

        let saved = dir.create_file("saved.txt", b"v1");
        watch.poll();
        std::fs::write(&saved, b"v2 - longer").expect("save");
        assert!(watch.poll(), "a save-in-place must be reported");
    }

    /// The browser's own re-read must not re-trigger the watcher: reading
    /// a directory produces `OPEN`/`ACCESS`/`CLOSE_NOWRITE`, none of which
    /// are subscribed, and a refresh that ping-ponged with the watcher
    /// would spin the whole application.
    #[test]
    fn reading_the_directory_reports_nothing() {
        let dir = TempDir::new("watch_quiet");
        dir.create_file("a.txt", b"x");
        dir.create_file("b.txt", b"x");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());
        watch.poll(); // drop the noise of the setup writes themselves

        let seen = std::fs::read_dir(dir.path()).expect("read").count();
        assert_eq!(seen, 2);
        assert!(!watch.poll(), "a plain directory read is not a change");
    }

    /// A directory deleted out from under the watcher is reported, so the
    /// browser can say so instead of silently showing a ghost.
    #[test]
    fn deleting_the_watched_directory_itself_is_reported() {
        let dir = TempDir::new("watch_self_delete");
        let file = dir.create_file("blocker.txt", b"x");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());

        std::fs::remove_file(&file).expect("empty the directory first");
        std::fs::remove_dir(dir.path()).expect("remove the watched directory");
        assert!(
            watch.poll(),
            "DELETE_SELF on the open directory must be reported"
        );
    }

    /// Navigating re-arms from scratch: a directory left behind must stop
    /// being watched, or its changes would refresh a window showing
    /// somewhere else.
    #[test]
    fn a_directory_left_behind_stops_being_watched() {
        let dir = TempDir::new("watch_rearm");
        let old = dir.create_dir("old");
        let new = dir.create_dir("new");
        let mut watch = DirsWatch::new();
        watch.follow(&old);
        watch.follow(&new);

        dir.create_file("old/late.txt", b"x");
        assert!(
            !watch.poll(),
            "changes in the abandoned directory must be ignored"
        );
        dir.create_file("new/fresh.txt", b"x");
        assert!(watch.poll(), "changes in the current directory must fire");
    }

    /// A watcher the kernel refused (or a path that cannot be watched)
    /// degrades to permanently quiet rather than to an error: `raw_fd` goes
    /// back to `None` and every poll is calm.
    #[test]
    fn a_lost_watch_degrades_to_quiet() {
        let dir = TempDir::new("watch_degrade");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());
        assert!(watch.fd().is_some());

        // A path that cannot exist: add_watch fails, the watcher goes
        // quiet, and nothing panics.
        watch.follow(&dir.path().join("gone"));
        assert!(watch.fd().is_none(), "an unarmed watch has no fd");
        assert!(!watch.poll());
    }

    /// Queue overflow and kernel receipt events travel in the mask's high
    /// bits: a receipt (`IN_IGNORED`) is not a change, overflow is.
    #[test]
    fn a_renamed_directory_moves_the_watch_with_it() {
        // MOVE_SELF: the directory the user is looking at was renamed by
        // another process. The stored path no longer matches the inode, so
        // the only honest response is a re-read (which will likely fail
        // and be reported) -- reported here as "something changed".
        let parent = TempDir::new("watch_move_self");
        let child = parent.create_dir("child");
        let mut watch = DirsWatch::new();
        watch.follow(&child);

        std::fs::rename(&child, parent.path().join("child2")).expect("rename the watched dir");
        assert!(
            watch.poll(),
            "the watched directory moving must be reported"
        );
    }

    /// Once the kernel has reaped the armed watch (the directory was
    /// deleted), the watcher must stop offering its fd: a poll(2) that
    /// kept waiting on it would block on a descriptor that can never
    /// carry a real event again.
    #[test]
    fn a_reaped_watch_loses_its_fd() {
        let dir = TempDir::new("watch_reaped_fd");
        let file = dir.create_file("blocker.txt", b"x");
        let mut watch = DirsWatch::new();
        watch.follow(dir.path());
        assert!(watch.fd().is_some());

        std::fs::remove_file(&file).expect("empty the directory first");
        std::fs::remove_dir(dir.path()).expect("remove the watched directory");
        assert!(watch.poll(), "the deletion is reported");
        assert!(watch.fd().is_none(), "the reaped watch has no fd to poll");
        assert!(!watch.poll(), "and stays quiet");
    }
}
