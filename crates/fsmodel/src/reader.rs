//! Turning a path into a list of [`Entry`]s.
//!
//! The guiding rule here is **partial success beats total failure**. A
//! directory read touches many independent inodes, and any one of them can
//! fail on its own (a race with a concurrent delete, a dangling automount, a
//! file whose name isn't valid UTF-8). Aborting the whole listing because
//! one entry misbehaved would make the browser unusable in exactly the
//! situations where the user most needs to look at the directory. So a
//! failed *entry* is skipped and counted; only a failed *`read_dir`* -- where
//! there is genuinely nothing to show -- is an error.

use crate::entry::Entry;
use std::path::{Path, PathBuf};

/// Why a directory couldn't be listed at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The path doesn't exist.
    NotFound,
    /// The path exists but we lack permission to read it -- distinguished
    /// from [`Self::NotFound`] because the UI should say "permission
    /// denied", not "no such directory", and the two suggest completely
    /// different user actions.
    PermissionDenied,
    /// The path exists but isn't a directory.
    NotADirectory,
    /// Anything else, carrying the OS message for display.
    Other(String),
}

impl ReadError {
    /// Classify a `std::io::Error` from `read_dir`.
    fn from_io(err: &std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            std::io::ErrorKind::NotADirectory => Self::NotADirectory,
            _ => Self::Other(err.to_string()),
        }
    }

    /// A short, user-facing description.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotFound => "directory not found".to_owned(),
            Self::PermissionDenied => "permission denied".to_owned(),
            Self::NotADirectory => "not a directory".to_owned(),
            Self::Other(msg) => msg.clone(),
        }
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ReadError {}

/// Everything one directory read produced.
#[derive(Debug, Clone)]
pub struct Listing {
    /// The directory that was read, canonicalised where possible.
    pub path: PathBuf,
    /// Entries in filesystem order -- **unsorted and unfiltered**. Applying
    /// a [`crate::sort::SortPolicy`] and [`crate::filter::Filter`] is the
    /// caller's job, so re-sorting doesn't mean re-reading.
    pub entries: Vec<Entry>,
    /// How many entries were skipped because they couldn't be stat'ed.
    ///
    /// Surfaced rather than silently swallowed so the UI can warn that the
    /// listing is incomplete -- silently showing 9 of 10 files is worse than
    /// saying so.
    pub skipped: usize,
}

impl Listing {
    /// Number of entries read.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the directory yielded no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reads directories into [`Listing`]s.
///
/// A unit struct rather than free functions: it gives the operation an
/// obvious owner and leaves room for read options (follow-mounts, max
/// entries, …) to land as fields without changing any call site.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirReader;

impl DirReader {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Read `path` into a [`Listing`].
    ///
    /// Individual unreadable entries are skipped and counted in
    /// [`Listing::skipped`]; only a failure to open the directory itself is
    /// returned as an error.
    pub fn read(self, path: &Path) -> Result<Listing, ReadError> {
        let read_dir = std::fs::read_dir(path).map_err(|e| ReadError::from_io(&e))?;

        let mut entries = Vec::new();
        let mut skipped = 0usize;

        for item in read_dir {
            // The DirEntry itself can fail mid-iteration (e.g. the directory
            // was removed while we were walking it).
            let Ok(dir_entry) = item else {
                skipped += 1;
                continue;
            };
            // Non-UTF-8 names exist on Unix. Rendering them correctly is a
            // rabbit hole (they're arbitrary bytes, not text); skipping and
            // counting is honest and keeps `Entry::name` a plain `String`.
            let Some(name) = dir_entry.file_name().to_str().map(str::to_owned) else {
                skipped += 1;
                continue;
            };
            // `DirEntry::metadata` does not follow symlinks, which is
            // exactly what `EntryKind` wants -- and it's often served from
            // the readdir buffer, avoiding a syscall per entry.
            let Ok(metadata) = dir_entry.metadata() else {
                skipped += 1;
                continue;
            };

            entries.push(Entry::from_parts(dir_entry.path(), name, &metadata));
        }

        Ok(Listing {
            // Canonicalise so `..` and symlinked paths present consistently;
            // fall back to the path as given if that fails, since a listing
            // we successfully read should never be discarded over cosmetics.
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            entries,
            skipped,
        })
    }

    /// The parent of `path`, or `None` at the filesystem root.
    ///
    /// Lives here so navigation doesn't hand-roll `Path::parent` handling
    /// and accidentally offer an "up" action at `/` that goes nowhere.
    #[must_use]
    pub fn parent_of(self, path: &Path) -> Option<PathBuf> {
        path.parent().map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::EntryKind;
    use crate::test_support::TempDir;

    fn sorted_names(listing: &Listing) -> Vec<String> {
        let mut names: Vec<String> = listing.entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn reads_every_entry() {
        let dir = TempDir::new("reader_basic");
        dir.create_file("a.txt", b"x");
        dir.create_file("b.txt", b"xx");
        dir.create_dir("sub");

        let listing = DirReader::new().read(dir.path()).expect("read");
        assert_eq!(listing.len(), 3);
        assert_eq!(listing.skipped, 0);
        assert_eq!(sorted_names(&listing), ["a.txt", "b.txt", "sub"]);
    }

    #[test]
    fn includes_hidden_entries_in_the_raw_listing() {
        let dir = TempDir::new("reader_hidden");
        dir.create_file(".hidden", b"x");
        dir.create_file("visible", b"x");

        // The reader reports everything; hiding is the Filter's job. If the
        // reader dropped dotfiles, "show hidden" could never work without
        // a re-read.
        let listing = DirReader::new().read(dir.path()).expect("read");
        assert_eq!(listing.len(), 2);
    }

    #[test]
    fn empty_directory_reads_cleanly() {
        let dir = TempDir::new("reader_empty");
        let listing = DirReader::new().read(dir.path()).expect("read");
        assert!(listing.is_empty());
        assert_eq!(listing.skipped, 0);
    }

    /// A broken symlink must not abort or reduce the listing -- it is a
    /// normal thing to find in a directory.
    #[test]
    fn broken_symlink_is_listed_not_skipped() {
        let dir = TempDir::new("reader_broken_link");
        dir.create_file("real", b"x");
        dir.create_symlink("dangling", "nowhere-at-all");

        let listing = DirReader::new().read(dir.path()).expect("read");
        assert_eq!(listing.len(), 2);
        assert_eq!(listing.skipped, 0, "a dangling link is not a failure");

        let link = listing
            .entries
            .iter()
            .find(|e| e.name == "dangling")
            .expect("dangling entry present");
        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[test]
    fn missing_directory_is_not_found() {
        let dir = TempDir::new("reader_missing");
        let err = DirReader::new()
            .read(&dir.path().join("no-such-dir"))
            .expect_err("must fail");
        assert_eq!(err, ReadError::NotFound);
    }

    /// Reading a *file* must be distinguishable from reading a missing
    /// path, so the UI can explain what actually went wrong.
    #[test]
    fn reading_a_file_reports_not_a_directory() {
        let dir = TempDir::new("reader_not_a_dir");
        dir.create_file("regular.txt", b"x");

        let err = DirReader::new()
            .read(&dir.path().join("regular.txt"))
            .expect_err("must fail");
        assert_eq!(err, ReadError::NotADirectory);
    }

    #[test]
    fn entry_paths_are_joined_against_the_directory() {
        let dir = TempDir::new("reader_paths");
        dir.create_file("child.txt", b"x");

        let listing = DirReader::new().read(dir.path()).expect("read");
        let entry = &listing.entries[0];
        assert!(entry.path.ends_with("child.txt"));
        assert!(entry.path.is_absolute(), "paths must be usable as-is");
    }

    #[test]
    fn parent_of_root_is_none() {
        assert!(DirReader::new().parent_of(Path::new("/")).is_none());
        assert_eq!(
            DirReader::new().parent_of(Path::new("/usr/share")),
            Some(PathBuf::from("/usr"))
        );
    }

    #[test]
    fn error_messages_are_distinct_and_non_empty() {
        for err in [
            ReadError::NotFound,
            ReadError::PermissionDenied,
            ReadError::NotADirectory,
        ] {
            assert!(!err.message().is_empty());
        }
        assert_ne!(
            ReadError::NotFound.message(),
            ReadError::PermissionDenied.message()
        );
    }
}
