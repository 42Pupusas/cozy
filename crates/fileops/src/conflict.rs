//! What to do when a copy or move would land on top of something that
//! already exists.
//!
//! Split out from the transfer engine because the *decision* and the
//! *mechanics* change for completely different reasons: the decision is
//! driven by the UI (a dialog, a remembered "apply to all" choice), the
//! mechanics by the filesystem. Keeping them apart lets every branch of the
//! decision be tested without touching a single inode.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// How to resolve a name collision at the destination.
///
/// `Ask` exists so the engine can hand the decision back to the caller
/// mid-operation instead of guessing. A file browser that guessed here would
/// eventually guess "overwrite" on something irreplaceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictPolicy {
    /// Stop and let the caller decide. The default, deliberately: the safe
    /// behaviour should be what you get when you don't think about it.
    #[default]
    Ask,
    /// Replace the existing entry.
    Overwrite,
    /// Leave the existing entry alone and skip this source.
    Skip,
    /// Keep both, giving the incoming entry a numbered name.
    Rename,
}

/// The outcome of applying a [`ConflictPolicy`] to one collision.
///
/// Separate from the policy because a policy of `Rename` produces a
/// *specific* path, and the engine needs that path, not the intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Proceed, replacing whatever is at the destination.
    Replace,
    /// Skip this entry entirely; it is not an error.
    Skip,
    /// Proceed, but write to this path instead.
    Use(PathBuf),
    /// The caller must decide; re-invoke with a concrete policy.
    Ask,
}

/// Resolves collisions, and remembers a name-generation scheme.
///
/// A struct rather than a bare function because "keep both" needs to
/// enumerate candidate names against the destination directory, and that
/// probing is stateful enough (and fiddly enough) to deserve an owner.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConflictResolver {
    policy: ConflictPolicy,
}

impl ConflictResolver {
    /// A resolver applying `policy` to every collision it sees.
    #[must_use]
    pub const fn new(policy: ConflictPolicy) -> Self {
        Self { policy }
    }

    /// The policy in force.
    #[must_use]
    pub const fn policy(&self) -> ConflictPolicy {
        self.policy
    }

    /// Decide what to do about `dest`, which may or may not already exist.
    ///
    /// Uses `symlink_metadata`, so a **broken symlink at the destination
    /// still counts as a collision**. Following the link instead would make
    /// a dangling link look like free space, and the write would then either
    /// fail confusingly or -- worse -- create the link's target somewhere
    /// entirely unrelated to where the user dropped the file.
    #[must_use]
    pub fn resolve(&self, dest: &Path) -> Resolution {
        if std::fs::symlink_metadata(dest).is_err() {
            // Nothing there: no conflict, use the requested path as-is.
            return Resolution::Use(dest.to_path_buf());
        }
        match self.policy {
            ConflictPolicy::Ask => Resolution::Ask,
            ConflictPolicy::Overwrite => Resolution::Replace,
            ConflictPolicy::Skip => Resolution::Skip,
            ConflictPolicy::Rename => Resolution::Use(Self::free_name(dest)),
        }
    }

    /// First unused variant of `dest`, as `name (1).ext`, `name (2).ext`, ...
    ///
    /// Matches what desktop file managers produce, so the results look
    /// familiar rather than like our invention.
    #[must_use]
    pub fn free_name(dest: &Path) -> PathBuf {
        let Some(parent) = dest.parent() else {
            return dest.to_path_buf();
        };
        let Some(name) = dest.file_name() else {
            return dest.to_path_buf();
        };
        let (stem, ext) = Self::split_name(name);
        // Bounded rather than `loop`: if a directory somehow contains every
        // candidate, spinning forever is worse than reporting the last one
        // and letting the write fail with a real errno.
        for n in 1..10_000u32 {
            let mut candidate = OsString::from(&stem);
            candidate.push(format!(" ({n})"));
            candidate.push(&ext);
            let full = parent.join(&candidate);
            if std::fs::symlink_metadata(&full).is_err() {
                return full;
            }
        }
        dest.to_path_buf()
    }

    /// Split a file name into the part a suffix goes *after* and the
    /// extension it goes *before*.
    ///
    /// The suffix has to land before the extension (`photo (1).png`, not
    /// `photo.png (1)`) or the copy stops being openable by the app that
    /// handles its type -- the same reasoning as the trash's dedup naming.
    ///
    /// Leading dots are never treated as an extension boundary, so
    /// `.bashrc` stays whole and becomes `.bashrc (1)`.
    fn split_name(name: &OsStr) -> (OsString, OsString) {
        let bytes = name.as_encoded_bytes();
        // Skip leading dots so dotfiles aren't read as "empty stem".
        let search_from = bytes.iter().position(|b| *b != b'.').unwrap_or(bytes.len());
        let dot = bytes[search_from..]
            .iter()
            .position(|b| *b == b'.')
            .map(|i| i + search_from);
        dot.map_or_else(
            || (name.to_os_string(), OsString::new()),
            |i| {
                // SAFETY: `as_encoded_bytes` bytes are split at an ASCII
                // `.`, which is never part of a multi-byte sequence in any
                // encoding `OsStr` uses, so both halves remain valid.
                let (a, b) = unsafe {
                    (
                        OsStr::from_encoded_bytes_unchecked(&bytes[..i]),
                        OsStr::from_encoded_bytes_unchecked(&bytes[i..]),
                    )
                };
                (a.to_os_string(), b.to_os_string())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn a_free_path_is_used_directly() {
        let dir = TempDir::new("conflict-free");
        let dest = dir.join("new.txt");
        let resolver = ConflictResolver::new(ConflictPolicy::Overwrite);
        assert_eq!(resolver.resolve(&dest), Resolution::Use(dest));
    }

    #[test]
    fn ask_is_the_default_policy() {
        // The safe answer must be the one you get for free.
        assert_eq!(ConflictPolicy::default(), ConflictPolicy::Ask);
        assert_eq!(ConflictResolver::default().policy(), ConflictPolicy::Ask);
    }

    #[test]
    fn an_existing_file_defers_to_the_caller_under_ask() {
        let dir = TempDir::new("conflict-ask");
        let dest = dir.create_file("taken.txt", b"old");
        let resolver = ConflictResolver::new(ConflictPolicy::Ask);
        assert_eq!(resolver.resolve(&dest), Resolution::Ask);
    }

    #[test]
    fn overwrite_replaces() {
        let dir = TempDir::new("conflict-overwrite");
        let dest = dir.create_file("taken.txt", b"old");
        let resolver = ConflictResolver::new(ConflictPolicy::Overwrite);
        assert_eq!(resolver.resolve(&dest), Resolution::Replace);
    }

    #[test]
    fn skip_leaves_the_original_alone() {
        let dir = TempDir::new("conflict-skip");
        let dest = dir.create_file("taken.txt", b"old");
        let resolver = ConflictResolver::new(ConflictPolicy::Skip);
        assert_eq!(resolver.resolve(&dest), Resolution::Skip);
    }

    #[test]
    fn rename_puts_the_suffix_before_the_extension() {
        // `photo (1).png` stays a PNG; `photo.png (1)` would not open.
        let dir = TempDir::new("conflict-rename");
        let dest = dir.create_file("photo.png", b"old");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join("photo (1).png"))
        );
    }

    #[test]
    fn rename_keeps_counting_past_taken_names() {
        let dir = TempDir::new("conflict-rename-many");
        let dest = dir.create_file("doc.txt", b"a");
        dir.create_file("doc (1).txt", b"b");
        dir.create_file("doc (2).txt", b"c");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join("doc (3).txt"))
        );
    }

    #[test]
    fn a_file_without_an_extension_just_gets_the_suffix() {
        let dir = TempDir::new("conflict-noext");
        let dest = dir.create_file("README", b"a");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join("README (1)"))
        );
    }

    #[test]
    fn a_dotfile_is_not_treated_as_all_extension() {
        // `.bashrc` must become `.bashrc (1)`, not ` (1).bashrc`.
        let dir = TempDir::new("conflict-dotfile");
        let dest = dir.create_file(".bashrc", b"a");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join(".bashrc (1)"))
        );
    }

    #[test]
    fn a_multi_part_extension_is_split_at_the_first_dot() {
        // Matches the trash's naming: the whole `.tar.gz` is preserved so
        // the archive still round-trips through the same tools.
        let dir = TempDir::new("conflict-tarball");
        let dest = dir.create_file("backup.tar.gz", b"a");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join("backup (1).tar.gz"))
        );
    }

    #[test]
    fn a_broken_symlink_still_counts_as_a_collision() {
        // The classic bug: `metadata` follows the link, sees nothing, and
        // reports free space -- then the write goes somewhere unexpected.
        let dir = TempDir::new("conflict-broken-link");
        let dest = dir.create_symlink("link.txt", "nowhere-at-all");
        let resolver = ConflictResolver::new(ConflictPolicy::Skip);
        assert_eq!(resolver.resolve(&dest), Resolution::Skip);
    }

    #[test]
    fn a_directory_collides_just_like_a_file() {
        let dir = TempDir::new("conflict-dir");
        let dest = dir.create_dir("subdir");
        let resolver = ConflictResolver::new(ConflictPolicy::Rename);
        assert_eq!(
            resolver.resolve(&dest),
            Resolution::Use(dir.join("subdir (1)"))
        );
    }
}
