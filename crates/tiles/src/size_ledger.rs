//! [`SizeLedger`] -- the directory totals known so far: final ones, and the
//! partial ones of walks still running.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use fsmodel::tally::Tallied;

/// What is known about one directory's total. `Stale` is a final total
/// from before the directory changed, shown until the recount passes it or
/// settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Unknown,
    Partial(u64),
    Stale(u64),
    Final(u64),
}

impl Measure {
    /// The bytes to rank by: what has been counted so far.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Unknown => 0,
            Self::Partial(bytes) | Self::Stale(bytes) | Self::Final(bytes) => bytes,
        }
    }

    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Final(_))
    }
}

/// Every directory total known so far, final, partial or stale.
///
/// Final totals stay until invalidated. Partial totals only grow, so a walk
/// restarted over the same contents never shrinks a tile it already grew.
/// An invalidated total is kept as stale and shown until its recount
/// overtakes it or settles.
#[derive(Debug, Default)]
pub struct SizeLedger {
    totals: HashMap<PathBuf, u64>,
    partial: HashMap<PathBuf, u64>,
    stale: HashMap<PathBuf, u64>,
}

impl SizeLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn measure(&self, path: &Path) -> Measure {
        if let Some(&bytes) = self.totals.get(path) {
            return Measure::Final(bytes);
        }
        let partial = self.partial.get(path).copied();
        match (self.stale.get(path).copied(), partial) {
            (Some(stale), Some(counted)) if counted > stale => Measure::Partial(counted),
            (Some(stale), _) => Measure::Stale(stale),
            (None, Some(counted)) => Measure::Partial(counted),
            (None, None) => Measure::Unknown,
        }
    }

    #[must_use]
    pub fn is_final(&self, path: &Path) -> bool {
        self.totals.contains_key(path)
    }

    /// Fold in one report. Returns whether the measure changed.
    pub fn record(&mut self, tallied: Tallied) -> bool {
        let before = self.measure(&tallied.path);
        if tallied.settled {
            self.partial.remove(&tallied.path);
            self.stale.remove(&tallied.path);
            self.totals.insert(tallied.path, tallied.bytes);
            return before != Measure::Final(tallied.bytes);
        }
        if self.totals.contains_key(&tallied.path) {
            return false;
        }
        let known = self.partial.entry(tallied.path.clone()).or_insert(0);
        *known = (*known).max(tallied.bytes);
        self.measure(&tallied.path) != before
    }

    /// Mark `path`'s total as out of date: kept on show, but no longer
    /// final. A path with only a partial count starts its count over.
    pub fn outdate(&mut self, path: &Path) {
        if let Some(bytes) = self.totals.remove(path) {
            self.stale.insert(path.to_path_buf(), bytes);
        }
        self.partial.remove(path);
    }

    /// Mark every child of `dir` out of date.
    pub fn outdate_children_of(&mut self, dir: &Path) {
        let children: Vec<PathBuf> = self
            .totals
            .keys()
            .chain(self.partial.keys())
            .filter(|path| path.parent() == Some(dir))
            .cloned()
            .collect();
        for child in children {
            self.outdate(&child);
        }
    }

    /// Drop everything known about `path`, for an entry that is gone.
    pub fn forget(&mut self, path: &Path) {
        self.totals.remove(path);
        self.partial.remove(path);
        self.stale.remove(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{Measure, SizeLedger};
    use fsmodel::tally::Tallied;
    use std::path::{Path, PathBuf};

    fn report(path: &str, bytes: u64, settled: bool) -> Tallied {
        Tallied {
            path: PathBuf::from(path),
            bytes,
            settled,
        }
    }

    #[test]
    fn an_unseen_directory_is_unknown_and_ranks_as_zero() {
        let ledger = SizeLedger::new();
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Unknown);
        assert_eq!(ledger.measure(Path::new("/a")).bytes(), 0);
    }

    #[test]
    fn partial_totals_grow_then_the_final_one_replaces_them() {
        let mut ledger = SizeLedger::new();
        assert!(ledger.record(report("/a", 10, false)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Partial(10));
        assert!(ledger.record(report("/a", 40, false)));
        assert!(ledger.record(report("/a", 55, true)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Final(55));
        assert!(ledger.is_final(Path::new("/a")));
    }

    #[test]
    fn a_smaller_partial_from_a_restarted_walk_is_ignored() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 40, false));
        assert!(!ledger.record(report("/a", 5, false)));
        assert_eq!(ledger.measure(Path::new("/a")).bytes(), 40);
    }

    #[test]
    fn a_partial_after_the_final_total_is_ignored() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 55, true));
        assert!(!ledger.record(report("/a", 70, false)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Final(55));
    }

    #[test]
    fn an_outdated_total_shows_until_the_recount_passes_it() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 100, true));
        ledger.outdate(Path::new("/a"));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Stale(100));
        assert!(!ledger.is_final(Path::new("/a")));
        assert!(
            !ledger.record(report("/a", 60, false)),
            "still shows the old total"
        );
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Stale(100));
        assert!(ledger.record(report("/a", 130, false)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Partial(130));
        assert!(ledger.record(report("/a", 150, true)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Final(150));
    }

    #[test]
    fn a_recount_that_settles_smaller_replaces_the_stale_total() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 100, true));
        ledger.outdate(Path::new("/a"));
        assert!(ledger.record(report("/a", 40, true)));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Final(40));
    }

    #[test]
    fn outdating_a_partial_count_starts_it_over() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 70, false));
        ledger.outdate(Path::new("/a"));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Unknown);
        assert!(ledger.record(report("/a", 5, false)));
    }

    #[test]
    fn outdating_a_folder_s_children_spares_other_folders() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/d/a", 1, true));
        ledger.record(report("/d/b", 2, false));
        ledger.record(report("/e/c", 3, true));
        ledger.outdate_children_of(Path::new("/d"));
        assert_eq!(ledger.measure(Path::new("/d/a")), Measure::Stale(1));
        assert_eq!(ledger.measure(Path::new("/d/b")), Measure::Unknown);
        assert_eq!(ledger.measure(Path::new("/e/c")), Measure::Final(3));
    }

    #[test]
    fn a_forgotten_path_is_unknown() {
        let mut ledger = SizeLedger::new();
        ledger.record(report("/a", 100, true));
        ledger.outdate(Path::new("/a"));
        ledger.forget(Path::new("/a"));
        assert_eq!(ledger.measure(Path::new("/a")), Measure::Unknown);
    }
}
