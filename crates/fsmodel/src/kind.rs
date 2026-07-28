//! What sort of thing a directory entry *is*.
//!
//! Deliberately classified from `symlink_metadata` (which does **not**
//! follow links), so a symlink reports as [`EntryKind::Symlink`] rather than
//! silently masquerading as its target. A file browser has to show the user
//! that a symlink is a symlink -- that's information they can't recover once
//! we've thrown it away, and it changes what deleting the entry means.
//!
//! Whether a symlink *points at* a directory is a separate question, tracked
//! alongside this by [`crate::entry::Entry::target_is_dir`], because
//! navigation cares about the target while rendering cares about the link.

use std::fs::FileType;

/// The kind of filesystem object a directory entry refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// A real directory (not a symlink to one).
    Dir,
    /// A regular file.
    File,
    /// A symbolic link, regardless of what it points at (or whether the
    /// target even exists -- broken links still list, and still need to be
    /// visible to the user so they can clean them up).
    Symlink,
    /// Anything else POSIX allows in a directory: fifo, socket, block or
    /// character device. Lumped together because a file browser treats them
    /// identically -- show them, don't pretend to open them.
    Other,
}

impl EntryKind {
    /// Classify from a [`FileType`] obtained via `symlink_metadata`.
    ///
    /// Note the ordering: `is_symlink` is checked *first*, because
    /// `std`'s `FileType` from a non-following stat reports symlink-ness
    /// exclusively, and we want that to win if the platform ever reports
    /// overlapping bits.
    #[must_use]
    pub fn from_file_type(ty: FileType) -> Self {
        if ty.is_symlink() {
            Self::Symlink
        } else if ty.is_dir() {
            Self::Dir
        } else if ty.is_file() {
            Self::File
        } else {
            Self::Other
        }
    }

    /// Whether this kind should sort into the "directories first" group.
    ///
    /// Only a genuine directory qualifies -- a symlink's grouping depends on
    /// its *target*, which this type intentionally doesn't know about, so
    /// [`crate::entry::Entry`] makes that call instead.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Dir)
    }

    /// A short, stable, lowercase label. Used for the "type" sort key and
    /// for any UI that wants a textual kind column, so that sorting by type
    /// and displaying the type never disagree.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier is driven entirely by `FileType`, which can't be
    /// constructed synthetically in std -- so exercise it against real
    /// filesystem objects created in a temp dir.
    #[test]
    fn classifies_real_filesystem_objects() {
        let dir = crate::test_support::TempDir::new("kind_classify");
        dir.create_dir("subdir");
        dir.create_file("regular.txt", b"hi");
        dir.create_symlink("link", "regular.txt");

        let meta = |name: &str| {
            std::fs::symlink_metadata(dir.path().join(name))
                .expect("symlink_metadata")
                .file_type()
        };

        assert_eq!(EntryKind::from_file_type(meta("subdir")), EntryKind::Dir);
        assert_eq!(
            EntryKind::from_file_type(meta("regular.txt")),
            EntryKind::File
        );
        // The crux: a symlink to a *file* must report Symlink, not File.
        assert_eq!(EntryKind::from_file_type(meta("link")), EntryKind::Symlink);
    }

    /// A symlink pointing at a directory must still classify as Symlink --
    /// this is the case that would regress if anyone reordered the checks
    /// in `from_file_type` to test `is_dir` first.
    #[test]
    fn symlink_to_dir_is_still_symlink() {
        let dir = crate::test_support::TempDir::new("kind_symlink_dir");
        dir.create_dir("target");
        dir.create_symlink("link_to_dir", "target");

        let ty = std::fs::symlink_metadata(dir.path().join("link_to_dir"))
            .expect("symlink_metadata")
            .file_type();
        assert_eq!(EntryKind::from_file_type(ty), EntryKind::Symlink);
    }

    #[test]
    fn only_real_dirs_group_as_directories() {
        assert!(EntryKind::Dir.is_dir());
        assert!(!EntryKind::Symlink.is_dir());
        assert!(!EntryKind::File.is_dir());
        assert!(!EntryKind::Other.is_dir());
    }

    #[test]
    fn labels_are_distinct() {
        let labels = [
            EntryKind::Dir.label(),
            EntryKind::File.label(),
            EntryKind::Symlink.label(),
            EntryKind::Other.label(),
        ];
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "labels must be distinguishable");
    }
}
