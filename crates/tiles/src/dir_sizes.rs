//! [`DirSizes`] -- the size each tile is ranked by: a file's own length, or a
//! directory's recursive total as [`SizeTally`] counts it.
//!
//! A directory ranks by its partial total while the walk runs, so its tile
//! grows as the count does. Totals are remembered per path across
//! navigations, so returning to a directory shows its last known sizes at
//! once. When some of a directory's entries change, only those are
//! recounted, and their old totals stay on show until the recount passes
//! them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use browse::ListingChange;
use fsmodel::entry::Entry;

use crate::size_ledger::{Measure, SizeLedger};
use crate::tally_roster::TallyRoster;

pub struct DirSizes {
    ledger: SizeLedger,
    roster: TallyRoster,
    revision: u64,
}

impl DirSizes {
    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            ledger: SizeLedger::new(),
            roster: TallyRoster::new(wake),
            revision: 0,
        }
    }

    /// Measure every directory among `entries` that has no final total.
    ///
    /// A request the running tallies already cover is left alone, so
    /// narrowing a filter does not restart them. Otherwise tallies of paths
    /// no longer wanted are cancelled, and the paths no tally covers get a
    /// new one.
    pub fn follow<'a>(&mut self, entries: impl Iterator<Item = &'a Entry>) {
        let wanted: Vec<&Path> = entries
            .filter(|entry| entry.kind.is_dir() && !self.ledger.is_final(&entry.path))
            .map(|entry| entry.path.as_path())
            .collect();
        if !wanted.is_empty() && wanted.iter().all(|path| self.roster.owns(path)) {
            return;
        }
        let unowned: Vec<PathBuf> = wanted
            .iter()
            .filter(|path| !self.roster.owns(path))
            .map(|path| path.to_path_buf())
            .collect();
        self.roster
            .keep_only(&wanted.into_iter().collect::<HashSet<_>>());
        self.roster.start(unowned);
    }

    /// Mark the totals `change` names among `dir`'s children out of date,
    /// so the next [`follow`](Self::follow) recounts them.
    pub fn invalidate(&mut self, dir: &Path, change: &ListingChange) {
        match change {
            ListingChange::Everything => {
                self.ledger.outdate_children_of(dir);
                self.roster.release_children_of(dir);
            }
            ListingChange::Entries(names) => {
                for name in names {
                    let path = dir.join(name);
                    self.ledger.outdate(&path);
                    self.roster.release(&path);
                }
            }
        }
        self.revision += 1;
    }

    /// Fold in every total, partial or final, that has arrived. Returns
    /// whether any changed a size, so the loop knows the tiles need
    /// reranking.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for tallied in self.roster.drain() {
            changed |= self.ledger.record(tallied);
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Bumped whenever a size changes or is forgotten.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// What is known of `entry`'s size. A file is always final at its own
    /// length.
    #[must_use]
    pub fn measure(&self, entry: &Entry) -> Measure {
        if entry.kind.is_dir() {
            self.ledger.measure(&entry.path)
        } else {
            Measure::Final(entry.size)
        }
    }

    /// The size `entry` is ranked by. A directory still being walked counts
    /// as what has been found so far, and one not yet reached as zero.
    #[must_use]
    pub fn of(&self, entry: &Entry) -> u64 {
        self.measure(entry).bytes()
    }

    /// Whether `entry` is a directory still waiting for its final total.
    #[must_use]
    pub fn is_pending(&self, entry: &Entry) -> bool {
        !self.measure(entry).is_final()
    }
}

#[cfg(test)]
mod tests {
    use super::DirSizes;
    use crate::size_ledger::Measure;
    use crate::test_support::TempDir;
    use browse::ListingChange;
    use fsmodel::entry::Entry;
    use std::ffi::OsString;

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
    fn a_folder_ranks_by_its_partial_total_while_still_pending() {
        let dir = TempDir::new("dir_sizes_partial");
        for i in 0..300 {
            dir.create_file(&format!("folder/{i:03}/f"), &[0; 64]);
        }
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        sizes.follow(listed.iter());
        let mut seen = Vec::new();
        while sizes.is_pending(&listed[0]) {
            assert!(wake.wait(5000), "the tally never signalled");
            wake.drain();
            sizes.poll();
            seen.push((sizes.of(&listed[0]), sizes.is_pending(&listed[0])));
        }
        assert_eq!(seen.last(), Some(&(300 * 64, false)));
        assert!(
            seen.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .all(|&(bytes, pending)| !pending || bytes < 300 * 64),
            "a pending folder never shows the full total: {seen:?}"
        );
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
        sizes.invalidate(dir.path(), &ListingChange::Everything);
        assert!(sizes.is_pending(&listed[0]));
        assert_eq!(sizes.of(&listed[0]), 5, "the old total shows meanwhile");
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert_eq!(sizes.of(&listed[0]), 55);
    }

    #[test]
    fn a_named_change_recounts_only_that_folder() {
        let dir = TempDir::new("dir_sizes_named");
        dir.create_file("one/a", &[0; 5]);
        dir.create_file("two/b", &[0; 7]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);

        dir.create_file("two/c", &[0; 20]);
        let change = ListingChange::Entries(vec![OsString::from("two")]);
        sizes.invalidate(dir.path(), &change);
        assert!(
            !sizes.is_pending(&listed[0]),
            "the untouched folder stays final"
        );
        assert_eq!(sizes.measure(&listed[1]), Measure::Stale(7));
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert_eq!(sizes.of(&listed[0]), 5);
        assert_eq!(sizes.of(&listed[1]), 27);
    }

    #[test]
    fn a_change_mid_walk_keeps_the_other_folders_counting() {
        let dir = TempDir::new("dir_sizes_mid_walk");
        for i in 0..200 {
            dir.create_file(&format!("big/{i:03}/f"), &[0; 10]);
        }
        dir.create_file("small/a", &[0; 3]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        sizes.follow(listed.iter());

        dir.create_file("small/b", &[0; 4]);
        sizes.invalidate(
            dir.path(),
            &ListingChange::Entries(vec![OsString::from("small")]),
        );
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        assert_eq!(sizes.of(&listed[0]), 2000);
        assert_eq!(
            sizes.of(&listed[1]),
            7,
            "the recount, not the stale tally, settles it"
        );
    }

    #[test]
    fn a_change_to_a_file_leaves_every_folder_final() {
        let dir = TempDir::new("dir_sizes_file_change");
        dir.create_file("folder/a", &[0; 5]);
        dir.create_file("loose", &[0; 1]);
        let wake = waker::Waker::new().expect("eventfd");
        let mut sizes = DirSizes::new(wake.clone());
        let listed = entries(&dir);
        sizes.follow(listed.iter());
        settle(&mut sizes, &wake, &listed);
        sizes.invalidate(
            dir.path(),
            &ListingChange::Entries(vec![OsString::from("loose")]),
        );
        assert!(!sizes.is_pending(&listed[0]));
    }
}
