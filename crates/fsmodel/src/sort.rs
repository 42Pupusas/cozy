//! How a directory listing is ordered.
//!
//! Split from the reader because sorting is re-applied far more often than
//! reading: clicking a column header or toggling direction must reorder the
//! *existing* entries, not trigger a fresh set of syscalls.

use crate::entry::Entry;
use std::cmp::Ordering;

/// Which field the listing is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Case-insensitive by name -- the default, and what users expect when
    /// they haven't asked for anything else.
    #[default]
    Name,
    /// By byte size. Directories have no meaningful size, so they keep
    /// their name ordering within the directory group.
    Size,
    /// By last-modified time, oldest first when ascending.
    Modified,
    /// By entry kind, then by name within each kind.
    Kind,
}

/// Ascending or descending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Ascending,
    Descending,
}

impl SortOrder {
    /// Flip the direction -- what clicking an already-active column header
    /// does.
    #[must_use]
    pub const fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }

    /// Apply this direction to an ascending comparison result.
    const fn apply(self, ordering: Ordering) -> Ordering {
        match self {
            Self::Ascending => ordering,
            Self::Descending => ordering.reverse(),
        }
    }
}

/// The complete ordering policy: which key, which direction, and whether
/// directories are pinned above files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortPolicy {
    pub key: SortKey,
    pub order: SortOrder,
    /// Group directories before files regardless of the sort key.
    ///
    /// On by default because it's near-universal in file managers, and
    /// because it survives the direction flip: reversing the sort should
    /// reverse the *entries*, not suddenly bury the directories at the
    /// bottom. Hence grouping is applied outside the reversal.
    pub dirs_first: bool,
}

impl Default for SortPolicy {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            order: SortOrder::Ascending,
            dirs_first: true,
        }
    }
}

impl SortPolicy {
    /// Order `entries` in place according to this policy.
    ///
    /// Uses a stable sort so entries that compare equal keep their previous
    /// relative order -- meaning a re-sort never gratuitously shuffles rows
    /// the user is looking at.
    pub fn apply(self, entries: &mut [Entry]) {
        entries.sort_by(|a, b| self.compare_entries(a, b));
    }

    /// Compare two entries under this policy.
    ///
    /// Public because callers that sort *references* or index pairs (like
    /// `DirectoryView`, which orders indices to avoid cloning entries)
    /// need the same comparison [`Self::apply`] uses -- reimplementing it
    /// at the call site would let the two orderings drift apart.
    #[must_use]
    pub fn compare_entries(self, a: &Entry, b: &Entry) -> Ordering {
        // Directory grouping is deliberately *outside* the ascending/
        // descending flip: users expect "reverse the order" to reverse the
        // list within its groups, not to move all directories to the end.
        if self.dirs_first {
            match (a.is_navigable(), b.is_navigable()) {
                (true, false) => return Ordering::Less,
                (false, true) => return Ordering::Greater,
                _ => {}
            }
        }

        self.order.apply(self.compare_by_key(a, b))
    }

    /// The key comparison, always expressed ascending; direction is applied
    /// by the caller.
    fn compare_by_key(self, a: &Entry, b: &Entry) -> Ordering {
        match self.key {
            SortKey::Name => Self::compare_names(a, b),
            // Every non-name key falls back to the name comparison on a tie,
            // so the result is a total order and the listing never depends on
            // the arbitrary order the filesystem handed us entries in.
            SortKey::Size => a.size.cmp(&b.size).then_with(|| Self::compare_names(a, b)),
            SortKey::Modified => {
                Self::compare_modified(a, b).then_with(|| Self::compare_names(a, b))
            }
            SortKey::Kind => a
                .kind
                .label()
                .cmp(b.kind.label())
                .then_with(|| Self::compare_names(a, b)),
        }
    }

    /// Case-insensitive name comparison, falling back to the case-sensitive
    /// one so that `README` and `readme` have a stable, defined order rather
    /// than comparing equal and landing wherever the sort left them.
    fn compare_names(a: &Entry, b: &Entry) -> Ordering {
        a.lower_name
            .cmp(&b.lower_name)
            .then_with(|| a.name.cmp(&b.name))
    }

    /// Compare modification times, sorting unknown-mtime entries last.
    ///
    /// "Last" rather than "epoch" so an unreadable timestamp doesn't fake a
    /// 1970 date that would look like real data to the user.
    fn compare_modified(a: &Entry, b: &Entry) -> Ordering {
        match (a.modified, b.modified) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// Build entries in a scratch dir, returning them in creation order so
    /// each test can assert on what the sort *changed*.
    fn entries_from(dir: &TempDir, names: &[&str]) -> Vec<Entry> {
        names
            .iter()
            .map(|n| Entry::from_path(&dir.path().join(n)).expect("entry"))
            .collect()
    }

    fn names_of(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn sorts_by_name_case_insensitively() {
        let dir = TempDir::new("sort_name");
        for n in ["banana", "Apple", "cherry"] {
            dir.create_file(n, b"x");
        }
        let mut entries = entries_from(&dir, &["banana", "Apple", "cherry"]);

        SortPolicy::default().apply(&mut entries);
        // Case-insensitive: "Apple" leads despite its uppercase 'A' sorting
        // before every lowercase letter in a byte comparison.
        assert_eq!(names_of(&entries), ["Apple", "banana", "cherry"]);
    }

    #[test]
    fn directories_group_before_files() {
        let dir = TempDir::new("sort_dirs_first");
        dir.create_file("aaa_file.txt", b"x");
        dir.create_dir("zzz_dir");
        let mut entries = entries_from(&dir, &["aaa_file.txt", "zzz_dir"]);

        SortPolicy::default().apply(&mut entries);
        assert_eq!(
            names_of(&entries),
            ["zzz_dir", "aaa_file.txt"],
            "a z-named dir must still precede an a-named file"
        );
    }

    /// The subtle one: reversing direction must reverse entries *within*
    /// the groups, and must not send directories to the bottom.
    #[test]
    fn descending_keeps_directories_first() {
        let dir = TempDir::new("sort_desc_groups");
        dir.create_dir("adir");
        dir.create_dir("bdir");
        dir.create_file("afile", b"x");
        dir.create_file("bfile", b"x");
        let mut entries = entries_from(&dir, &["adir", "bdir", "afile", "bfile"]);

        SortPolicy {
            order: SortOrder::Descending,
            ..SortPolicy::default()
        }
        .apply(&mut entries);

        assert_eq!(names_of(&entries), ["bdir", "adir", "bfile", "afile"]);
    }

    #[test]
    fn dirs_first_can_be_disabled() {
        let dir = TempDir::new("sort_no_grouping");
        dir.create_file("aaa_file.txt", b"x");
        dir.create_dir("zzz_dir");
        let mut entries = entries_from(&dir, &["aaa_file.txt", "zzz_dir"]);

        SortPolicy {
            dirs_first: false,
            ..SortPolicy::default()
        }
        .apply(&mut entries);

        assert_eq!(names_of(&entries), ["aaa_file.txt", "zzz_dir"]);
    }

    #[test]
    fn sorts_by_size_with_name_tiebreak() {
        let dir = TempDir::new("sort_size");
        dir.create_file("big", b"aaaaaaaaaa");
        dir.create_file("small", b"a");
        // Equal sizes must fall back to name order, not filesystem order.
        dir.create_file("tie_b", b"aaa");
        dir.create_file("tie_a", b"aaa");
        let mut entries = entries_from(&dir, &["big", "small", "tie_b", "tie_a"]);

        SortPolicy {
            key: SortKey::Size,
            ..SortPolicy::default()
        }
        .apply(&mut entries);

        assert_eq!(names_of(&entries), ["small", "tie_a", "tie_b", "big"]);
    }

    #[test]
    fn sorts_by_modified_time() {
        let dir = TempDir::new("sort_mtime");
        dir.create_file("first", b"x");
        // Sleep past filesystem timestamp granularity so the ordering is
        // real rather than a coin flip between two identical mtimes.
        std::thread::sleep(std::time::Duration::from_millis(20));
        dir.create_file("second", b"x");
        let mut entries = entries_from(&dir, &["second", "first"]);

        SortPolicy {
            key: SortKey::Modified,
            ..SortPolicy::default()
        }
        .apply(&mut entries);

        assert_eq!(names_of(&entries), ["first", "second"]);
    }

    /// Symlinks-to-directories are navigable, so `dirs_first` must group
    /// them with directories -- otherwise the grouping contradicts what
    /// activating the entry actually does.
    #[test]
    fn navigable_symlinks_group_with_directories() {
        let dir = TempDir::new("sort_symlink_group");
        dir.create_dir("target");
        dir.create_file("aaa_file", b"x");
        dir.create_symlink("zzz_link", "target");
        let mut entries = entries_from(&dir, &["aaa_file", "zzz_link"]);

        SortPolicy::default().apply(&mut entries);
        assert_eq!(names_of(&entries), ["zzz_link", "aaa_file"]);
    }

    #[test]
    fn order_toggles() {
        assert_eq!(SortOrder::Ascending.toggled(), SortOrder::Descending);
        assert_eq!(SortOrder::Descending.toggled(), SortOrder::Ascending);
        assert_eq!(
            SortOrder::Ascending.toggled().toggled(),
            SortOrder::Ascending
        );
    }
}
