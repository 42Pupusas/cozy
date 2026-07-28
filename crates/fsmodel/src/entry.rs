//! One directory entry, with the metadata a browser actually renders and
//! sorts by.
//!
//! Metadata is captured **once**, at read time, rather than being re-stat'ed
//! per frame: a directory listing is redrawn constantly (scroll, hover,
//! selection) and re-stat'ing thousands of entries at 60fps would dominate
//! the frame budget. Staleness is handled by re-reading the directory when
//! inotify says something changed, not by polling.

use crate::kind::EntryKind;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A single entry in a directory, as captured at read time.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Full path to the entry. Kept (rather than just the file name) so
    /// callers can act on an entry without re-joining it against a parent
    /// they'd have to thread through separately.
    pub path: PathBuf,
    /// The entry's own file name, without any directory components.
    pub name: String,
    /// What kind of object this is, from a *non-following* stat -- so a
    /// symlink reads as [`EntryKind::Symlink`], not as its target.
    pub kind: EntryKind,
    /// Size in bytes. For a directory this is the meaningless on-disk size
    /// of the directory inode itself, so UIs should not show it -- computing
    /// a real recursive size is a separate, explicitly-requested operation.
    pub size: u64,
    /// Last modification time, or `None` when the platform/filesystem
    /// doesn't report one. Optional rather than defaulted so sorting can put
    /// unknown-mtime entries in a deliberate place instead of pretending
    /// they were modified at the epoch.
    pub modified: Option<SystemTime>,
    /// Whether the name begins with `.` -- the Unix hidden-file convention.
    /// Precomputed because the filter checks it for every entry on every
    /// re-filter, and it never changes for a given entry.
    pub hidden: bool,
    /// For a symlink, whether its *target* resolves to a directory.
    /// `false` for non-symlinks and for broken links.
    ///
    /// This is the navigation-vs-rendering split: [`Self::kind`] says what to
    /// *draw* (a link), this says what happens when the user *activates* it
    /// (descend, or open as a file).
    pub target_is_dir: bool,
}

impl Entry {
    /// Build an entry from a path, its non-following metadata, and its name.
    ///
    /// Takes `metadata` rather than stat-ing internally so the caller can
    /// reuse the `std::fs::DirEntry` metadata it already has, avoiding a
    /// second syscall per entry.
    #[must_use]
    pub fn from_parts(path: PathBuf, name: String, metadata: &Metadata) -> Self {
        let kind = EntryKind::from_file_type(metadata.file_type());
        // Only pay for the extra (following) stat on actual symlinks; for
        // everything else the answer is trivially false.
        let target_is_dir = kind == EntryKind::Symlink
            && std::fs::metadata(&path).is_ok_and(|target| target.is_dir());

        Self {
            hidden: name.starts_with('.'),
            name,
            kind,
            size: metadata.len(),
            // `modified` fails on platforms/filesystems without mtime; treat
            // that as "unknown" rather than propagating an error, since one
            // odd file shouldn't fail the whole directory read.
            modified: metadata.modified().ok(),
            target_is_dir,
            path,
        }
    }

    /// Read one entry directly from a path, stat-ing it here.
    ///
    /// The convenience path for callers that have a path but no `DirEntry`
    /// (e.g. resolving `..`). Returns `None` if the path can't be stat'ed or
    /// has no file name.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_owned();
        let metadata = std::fs::symlink_metadata(path).ok()?;
        Some(Self::from_parts(path.to_path_buf(), name, &metadata))
    }

    /// Whether activating this entry should *navigate into* it: a real
    /// directory, or a symlink resolving to one.
    ///
    /// This is the question navigation should ask -- never `kind.is_dir()`,
    /// which would refuse to follow directory symlinks.
    #[must_use]
    pub const fn is_navigable(&self) -> bool {
        self.kind.is_dir() || self.target_is_dir
    }

    /// Whether this is a symlink whose target doesn't resolve.
    ///
    /// Worth surfacing distinctly in the UI: a broken link looks like a
    /// normal entry but every operation on it will behave surprisingly.
    #[must_use]
    pub fn is_broken_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink && std::fs::metadata(&self.path).is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn captures_name_size_and_hidden_flag() {
        let dir = TempDir::new("entry_basic");
        dir.create_file("visible.txt", b"12345");

        let entry = Entry::from_path(&dir.path().join("visible.txt")).expect("entry");
        assert_eq!(entry.name, "visible.txt");
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size, 5, "size must reflect the bytes written");
        assert!(!entry.hidden);
        assert!(entry.modified.is_some());
    }

    #[test]
    fn dotfiles_are_hidden() {
        let dir = TempDir::new("entry_hidden");
        dir.create_file(".bashrc", b"x");

        let entry = Entry::from_path(&dir.path().join(".bashrc")).expect("entry");
        assert!(entry.hidden);
    }

    /// A symlink to a directory must render as a link but navigate as a
    /// directory -- the whole reason `target_is_dir` exists separately.
    #[test]
    fn symlink_to_dir_renders_as_link_but_navigates_as_dir() {
        let dir = TempDir::new("entry_symlink_dir");
        dir.create_dir("real_dir");
        dir.create_symlink("link", "real_dir");

        let entry = Entry::from_path(&dir.path().join("link")).expect("entry");
        assert_eq!(entry.kind, EntryKind::Symlink, "must not masquerade as Dir");
        assert!(entry.target_is_dir);
        assert!(entry.is_navigable());
        assert!(!entry.is_broken_symlink());
    }

    #[test]
    fn symlink_to_file_is_not_navigable() {
        let dir = TempDir::new("entry_symlink_file");
        dir.create_file("target.txt", b"x");
        dir.create_symlink("link", "target.txt");

        let entry = Entry::from_path(&dir.path().join("link")).expect("entry");
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert!(!entry.target_is_dir);
        assert!(!entry.is_navigable());
    }

    /// Broken links are a real, common state (deleted target, dangling
    /// relative link). They must still produce an entry -- refusing to list
    /// them would hide files the user needs to clean up.
    #[test]
    fn broken_symlink_still_produces_an_entry() {
        let dir = TempDir::new("entry_broken_symlink");
        dir.create_symlink("dangling", "nonexistent-target");

        let entry = Entry::from_path(&dir.path().join("dangling")).expect("entry");
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert!(!entry.target_is_dir);
        assert!(!entry.is_navigable());
        assert!(entry.is_broken_symlink());
    }

    #[test]
    fn real_dir_is_navigable() {
        let dir = TempDir::new("entry_real_dir");
        dir.create_dir("sub");

        let entry = Entry::from_path(&dir.path().join("sub")).expect("entry");
        assert_eq!(entry.kind, EntryKind::Dir);
        assert!(entry.is_navigable());
        assert!(!entry.is_broken_symlink());
    }

    #[test]
    fn missing_path_yields_none() {
        let dir = TempDir::new("entry_missing");
        assert!(Entry::from_path(&dir.path().join("does-not-exist")).is_none());
    }
}
