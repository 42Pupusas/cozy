use std::collections::VecDeque;

use crate::listing_change::ListingChange;

/// The changes behind the browser's most recent refreshes, numbered by the
/// refresh count, so a reader that last looked at refresh `n` can ask what
/// changed since without the browser tracking its readers.
#[derive(Debug, Clone)]
pub struct ChangeLog {
    first: u64,
    recent: VecDeque<ListingChange>,
}

impl ChangeLog {
    const KEPT: usize = 32;

    /// An empty log whose next record is refresh `at + 1`.
    #[must_use]
    pub const fn new(at: u64) -> Self {
        Self {
            first: at,
            recent: VecDeque::new(),
        }
    }

    /// Forget every record, e.g. on moving to another directory.
    pub fn reset(&mut self, at: u64) {
        self.first = at;
        self.recent.clear();
    }

    pub fn record(&mut self, change: ListingChange) {
        if self.recent.len() == Self::KEPT {
            self.recent.pop_front();
            self.first += 1;
        }
        self.recent.push_back(change);
    }

    /// Everything recorded after refresh `seen`. A `seen` older than the log
    /// reaches back to changes no longer kept, so it gets
    /// [`ListingChange::Everything`].
    #[must_use]
    pub fn since(&self, seen: u64) -> ListingChange {
        if seen < self.first {
            return ListingChange::Everything;
        }
        let skip = usize::try_from(seen - self.first).unwrap_or(usize::MAX);
        let mut merged = ListingChange::none();
        for change in self.recent.iter().skip(skip) {
            merged.merge(change);
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::ChangeLog;
    use crate::listing_change::ListingChange;
    use std::ffi::OsString;

    fn names(list: &[&str]) -> ListingChange {
        ListingChange::Entries(list.iter().map(OsString::from).collect())
    }

    #[test]
    fn changes_after_the_seen_refresh_are_merged() {
        let mut log = ChangeLog::new(0);
        log.record(names(&["a"]));
        log.record(names(&["b"]));
        log.record(names(&["c"]));
        assert_eq!(log.since(1), names(&["b", "c"]));
        assert_eq!(log.since(0), names(&["a", "b", "c"]));
        assert!(log.since(3).is_empty());
    }

    #[test]
    fn a_reader_older_than_the_log_gets_everything() {
        let mut log = ChangeLog::new(0);
        for _ in 0..ChangeLog::KEPT + 3 {
            log.record(names(&["x"]));
        }
        assert_eq!(log.since(1), ListingChange::Everything);
        assert_eq!(log.since(3), names(&["x"]));
    }

    #[test]
    fn a_reset_starts_numbering_from_the_given_refresh() {
        let mut log = ChangeLog::new(0);
        log.record(names(&["a"]));
        log.reset(1);
        assert!(log.since(1).is_empty());
        log.record(names(&["b"]));
        assert_eq!(log.since(1), names(&["b"]));
        assert_eq!(log.since(0), ListingChange::Everything);
    }
}
