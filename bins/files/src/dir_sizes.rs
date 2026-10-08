//! [`DirSizes`] -- the size each tile is ranked by: a file's own length, or a
//! directory's recursive total once [`SizeTally`] has measured it.
//!
//! Totals are remembered per path across navigations, so returning to a
//! directory shows its last known sizes at once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use fsmodel::entry::Entry;
use fsmodel::tally::SizeTally;

pub struct DirSizes {
    wake: waker::Waker,
    totals: HashMap<PathBuf, u64>,
    queued: HashSet<PathBuf>,
    tally: Option<SizeTally>,
    revision: u64,
}

impl DirSizes {
    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            wake,
            totals: HashMap::new(),
            queued: HashSet::new(),
            tally: None,
            revision: 0,
        }
    }

    /// Measure every directory among `entries` that has no total yet.
    ///
    /// A request the running walk already covers is left alone, so narrowing
    /// a filter does not restart it. Anything else replaces the walk, and
    /// dropping the old one cancels it.
    pub fn follow<'a>(&mut self, entries: impl Iterator<Item = &'a Entry>) {
        let wanted: Vec<PathBuf> = entries
            .filter(|entry| entry.kind.is_dir() && !self.totals.contains_key(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();
        if wanted.is_empty() {
            self.tally = None;
            self.queued.clear();
            return;
        }
        if wanted.iter().all(|path| self.queued.contains(path)) {
            return;
        }
        self.queued = wanted.iter().cloned().collect();
        let wake = self.wake.clone();
        self.tally = SizeTally::start(wanted, move || wake.signal())
            .inspect_err(|error| eprintln!("files: size tally: {error}"))
            .ok();
    }

    /// Forget the totals of `dir`'s children, after its contents changed.
    pub fn invalidate(&mut self, dir: &Path) {
        self.totals.retain(|path, _| path.parent() != Some(dir));
        self.queued.clear();
        self.tally = None;
        self.revision += 1;
    }

    /// Fold in every total that has arrived. Returns whether any did, so the
    /// loop knows the tiles need reranking.
    pub fn poll(&mut self) -> bool {
        let Some(tally) = &self.tally else {
            return false;
        };
        let arrived = tally.drain();
        let changed = !arrived.is_empty();
        for tallied in arrived {
            self.queued.remove(&tallied.path);
            self.totals.insert(tallied.path, tallied.bytes);
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Bumped whenever a total arrives or is forgotten.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The size `entry` is ranked by. A directory not yet measured counts as
    /// zero, so it sorts behind everything known until its total arrives.
    #[must_use]
    pub fn of(&self, entry: &Entry) -> u64 {
        if entry.kind.is_dir() {
            self.totals.get(&entry.path).copied().unwrap_or(0)
        } else {
            entry.size
        }
    }

    /// Whether `entry` is a directory still waiting for its total.
    #[must_use]
    pub fn is_pending(&self, entry: &Entry) -> bool {
        entry.kind.is_dir() && !self.totals.contains_key(&entry.path)
    }
}

#[cfg(test)]
mod tests {
    use super::DirSizes;
    use crate::test_support::TempDir;
    use fsmodel::entry::Entry;

    fn entries(dir: &TempDir) -> Vec<Entry> {
        let mut found: Vec<Entry> = std::fs::read_dir(dir.path())
            .expect("read")
            .flatten()
            .filter_map(|item| Entry::from_path(&item.path()))
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    fn settle(sizes: &mut DirSizes, wake: &waker::Waker, entries: &[Entry]) {
        while entries.iter().any(|entry| sizes.is_pending(entry)) {
            assert!(wake.wait(5000), "the tally never signalled");
            wake.drain();
            sizes.poll();
        }
    }

    #[test]
    fn files_are_their_own_length_and_directories_their_total() {
        let dir = TempDir::new("dir_sizes_basic");
        dir.create_file("loose", &[0; 12]);
        dir.create_file("folder/a", &[0; 30]);
        dir.create_file("folder/nested/b", &[0; 70]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);

        assert!(sizes.is_pending(&listed[0]), "an unmeasured folder");
        assert_eq!(sizes.of(&listed[0]), 0);
        assert_eq!(sizes.of(&listed[1]), 12);

        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert_eq!(sizes.of(&listed[0]), 100);
    }

    #[test]
    fn arrivals_bump_the_revision() {
        let dir = TempDir::new("dir_sizes_revision");
        dir.create_file("folder/a", &[0; 5]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        let before = sizes.revision();
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert!(sizes.revision() > before);
    }

    #[test]
    fn measured_directories_are_not_walked_again() {
        let dir = TempDir::new("dir_sizes_twice");
        dir.create_file("folder/a", &[0; 5]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);

        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        sizes.follow(listed.iter());
        assert!(
            !sizes.poll(),
            "a second follow must not queue a second walk"
        );
    }

    #[test]
    fn a_narrower_request_keeps_the_running_walk() {
        let dir = TempDir::new("dir_sizes_narrow");
        dir.create_file("one/a", &[0; 5]);
        dir.create_file("two/b", &[0; 7]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);

        sizes.follow(listed.iter());
        sizes.follow(listed.iter().take(1));
        settle(&mut sizes, &wake, &listed);
        assert_eq!(
            sizes.of(&listed[1]),
            7,
            "the second folder was still measured"
        );
    }

    #[test]
    fn invalidating_remeasures_changed_contents() {
        let dir = TempDir::new("dir_sizes_invalidate");
        dir.create_file("folder/a", &[0; 5]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);

        dir.create_file("folder/b", &[0; 50]);
        sizes.invalidate(dir.path());
        assert!(sizes.is_pending(&listed[0]));
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert_eq!(sizes.of(&listed[0]), 55);
    }
}
