//! Measuring a set of paths *before* transferring them.
//!
//! Two independent reasons this is its own pass rather than something the
//! transfer does as it goes:
//!
//! * A progress bar needs a denominator up front, or it can only spin.
//! * A free-space check needs the total before the first byte is written,
//!   so we can refuse cleanly instead of filling the disk and failing
//!   half-way through.

use crate::progress::Progress;
use std::collections::HashSet;
use std::path::Path;

/// What a survey found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Survey {
    /// Total bytes of regular file content.
    pub bytes: u64,
    /// Number of entries: regular files, directories, and symlinks.
    pub files: u64,
    /// Entries that could not be measured (vanished mid-walk, unreadable).
    ///
    /// Reported rather than fatal: a survey is an *estimate*, and refusing
    /// to copy 10,000 files because one is unreadable would be obnoxious.
    /// The transfer will hit the same entry and report it properly.
    pub skipped: u64,
}

impl Survey {
    /// Whether anything at all was measured.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files == 0
    }
}

/// Walks a set of paths and adds up what a transfer would move.
///
/// A struct because the walk carries state the recursion must share: a
/// cancellation handle, and the set of directories already visited.
pub struct Surveyor<'a> {
    progress: Option<&'a Progress>,
    /// Device+inode of every directory entered, so a symlink loop or a
    /// bind-mounted subtree cannot make the walk run forever.
    seen_dirs: HashSet<(u64, u64)>,
    result: Survey,
}

impl<'a> Surveyor<'a> {
    /// A surveyor reporting nothing and never cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            progress: None,
            seen_dirs: HashSet::new(),
            result: Survey::default(),
        }
    }

    /// A surveyor that checks `progress` for cancellation as it walks.
    ///
    /// Surveying a huge tree is itself slow enough to need aborting -- the
    /// user should not have to wait out a measurement of `/` before their
    /// cancel takes effect.
    #[must_use]
    pub fn with_progress(progress: &'a Progress) -> Self {
        Self {
            progress: Some(progress),
            seen_dirs: HashSet::new(),
            result: Survey::default(),
        }
    }

    /// Measure every path in `sources`, following none of them as links.
    #[must_use]
    pub fn run<I, P>(mut self, sources: I) -> Survey
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for source in sources {
            if self.cancelled() {
                break;
            }
            self.visit(source.as_ref());
        }
        self.result
    }

    /// Whether the caller has asked us to stop.
    fn cancelled(&self) -> bool {
        self.progress.is_some_and(Progress::is_cancelled)
    }

    /// Measure one entry, recursing into directories.
    fn visit(&mut self, path: &Path) {
        if self.cancelled() {
            return;
        }
        // `symlink_metadata`: a symlink counts as one entry of its own, and
        // is never followed. Following would double-count a link to an
        // already-included file, and could wander clean out of the tree.
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            self.result.skipped += 1;
            return;
        };
        self.result.files += 1;

        if meta.is_symlink() {
            return;
        }
        if meta.is_file() {
            self.result.bytes += meta.len();
            return;
        }
        if !meta.is_dir() {
            // A socket, fifo, or device node: it counts as an entry but has
            // no content a copy would transfer.
            return;
        }
        if !self.enter(&meta) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            // The directory itself was counted; its unreadable contents are
            // one unknown quantity, not an error.
            self.result.skipped += 1;
            return;
        };
        for entry in entries {
            match entry {
                Ok(entry) => self.visit(&entry.path()),
                Err(_) => self.result.skipped += 1,
            }
        }
    }

    /// Record a directory as visited, returning `false` if we've been here
    /// before -- the loop guard.
    fn enter(&mut self, meta: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        self.seen_dirs.insert((meta.dev(), meta.ino()))
    }
}

impl Default for Surveyor<'_> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn an_empty_set_measures_nothing() {
        let survey = Surveyor::new().run(Vec::<std::path::PathBuf>::new());
        assert!(survey.is_empty());
        assert_eq!(survey.bytes, 0);
    }

    #[test]
    fn a_single_file_reports_its_size() {
        let dir = TempDir::new("survey-one");
        let file = dir.create_file("a.txt", b"hello");
        let survey = Surveyor::new().run([&file]);
        assert_eq!(survey.files, 1);
        assert_eq!(survey.bytes, 5);
        assert_eq!(survey.skipped, 0);
    }

    #[test]
    fn a_tree_is_summed_recursively() {
        let dir = TempDir::new("survey-tree");
        dir.create_file("top.txt", b"1234");
        dir.create_file("sub/nested.txt", b"123456");
        let survey = Surveyor::new().run([dir.path()]);
        // root dir + top.txt + sub dir + nested.txt
        assert_eq!(survey.files, 4);
        assert_eq!(survey.bytes, 10);
    }

    #[test]
    fn directories_count_as_entries_but_contribute_no_bytes() {
        // An 800-directory tree still shows progress even at zero bytes.
        let dir = TempDir::new("survey-dirs");
        dir.create_dir("a/b/c");
        let survey = Surveyor::new().run([dir.path()]);
        assert_eq!(survey.files, 4);
        assert_eq!(survey.bytes, 0);
    }

    #[test]
    fn a_symlink_is_one_entry_and_is_not_followed() {
        // Following would count the 5-byte target twice.
        let dir = TempDir::new("survey-link");
        dir.create_file("real.txt", b"hello");
        dir.create_symlink("link.txt", "real.txt");
        let survey = Surveyor::new().run([dir.path()]);
        assert_eq!(survey.files, 3);
        assert_eq!(survey.bytes, 5);
    }

    #[test]
    fn a_broken_symlink_is_counted_not_skipped() {
        // It is a real entry that a copy will faithfully recreate.
        let dir = TempDir::new("survey-broken");
        dir.create_symlink("link.txt", "nowhere");
        let survey = Surveyor::new().run([dir.path()]);
        assert_eq!(survey.files, 2);
        assert_eq!(survey.skipped, 0);
    }

    #[test]
    fn a_symlink_loop_terminates() {
        // The reason `seen_dirs` exists: `a -> ..` would otherwise recurse
        // until the stack blew.
        let dir = TempDir::new("survey-loop");
        dir.create_dir("sub");
        dir.create_symlink("sub/up", "..");
        let survey = Surveyor::new().run([dir.path()]);
        assert!(survey.files >= 2);
    }

    #[test]
    fn a_missing_path_is_skipped_rather_than_fatal() {
        let dir = TempDir::new("survey-missing");
        let survey = Surveyor::new().run([dir.join("nope.txt")]);
        assert_eq!(survey.skipped, 1);
        assert_eq!(survey.files, 0);
    }

    #[test]
    fn several_sources_are_summed_together() {
        let dir = TempDir::new("survey-multi");
        let a = dir.create_file("a.txt", b"aa");
        let b = dir.create_file("b.txt", b"bbb");
        let survey = Surveyor::new().run([&a, &b]);
        assert_eq!(survey.files, 2);
        assert_eq!(survey.bytes, 5);
    }

    #[test]
    fn cancelling_stops_the_walk_early() {
        let dir = TempDir::new("survey-cancel");
        for i in 0..50 {
            dir.create_file(&format!("f{i}.txt"), b"xxxx");
        }
        let progress = Progress::new();
        progress.cancel();
        let survey = Surveyor::with_progress(&progress).run([dir.path()]);
        assert!(survey.is_empty(), "cancelled survey should not walk");
    }
}
