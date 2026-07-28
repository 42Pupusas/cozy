//! Copying and moving files and directory trees.
//!
//! # Why a move is not just "copy then delete"
//!
//! A move within one filesystem is a `rename(2)`: atomic, instant, and it
//! preserves everything (hard links, permissions, xattrs, inode identity).
//! Only when `rename` fails with `EXDEV` -- the destination is on another
//! filesystem -- must we fall back to copy-then-delete. That fallback is
//! where data loss lives, so the source is deleted **only after** its copy
//! is fully written and verified to exist.
//!
//! # Symlinks are recreated, never followed
//!
//! Copying a symlink copies *the link*. Following it would silently inflate
//! a 40-byte link into a 4 GB duplicate of its target, and a link pointing
//! outside the tree would drag in files the user never selected.

use crate::conflict::{ConflictResolver, Resolution};
use crate::progress::Progress;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Why a transfer failed.
#[derive(Debug)]
pub enum TransferError {
    /// The source path doesn't exist.
    NotFound(PathBuf),
    /// The destination already exists and the policy was [`Ask`].
    ///
    /// [`Ask`]: crate::conflict::ConflictPolicy::Ask
    Conflict(PathBuf),
    /// Refusing to copy a directory into itself or into its own descendant,
    /// which would recurse until the disk filled.
    WouldRecurse(PathBuf, PathBuf),
    /// The user asked to stop. Not really an error, but it must interrupt
    /// the same paths an error does.
    Cancelled,
    /// The filesystem refused an operation.
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "{} does not exist", p.display()),
            Self::Conflict(p) => write!(f, "{} already exists", p.display()),
            Self::WouldRecurse(s, d) => write!(
                f,
                "cannot copy {} into itself ({})",
                s.display(),
                d.display()
            ),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl std::error::Error for TransferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(_, e) => Some(e),
            _ => None,
        }
    }
}

/// What one transfer actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transferred {
    /// Where the entry ended up. Differs from the requested path when the
    /// conflict policy renamed it.
    pub dest: PathBuf,
    /// True when the conflict policy said skip, so nothing was written.
    pub skipped: bool,
}

/// Copies and moves paths, applying a conflict policy and reporting progress.
#[derive(Debug, Clone)]
pub struct Transfer {
    resolver: ConflictResolver,
    progress: Progress,
}

impl Transfer {
    /// A transfer using `resolver` for collisions and `progress` for
    /// reporting and cancellation.
    #[must_use]
    pub const fn new(resolver: ConflictResolver, progress: Progress) -> Self {
        Self { resolver, progress }
    }

    /// Copy `source` into the directory `dest_dir`, keeping its name.
    ///
    /// # Errors
    /// See [`TransferError`].
    pub fn copy_into(&self, source: &Path, dest_dir: &Path) -> Result<Transferred, TransferError> {
        let dest = Self::dest_for(source, dest_dir)?;
        self.copy(source, &dest)
    }

    /// Move `source` into the directory `dest_dir`, keeping its name.
    ///
    /// # Errors
    /// See [`TransferError`].
    pub fn move_into(&self, source: &Path, dest_dir: &Path) -> Result<Transferred, TransferError> {
        let dest = Self::dest_for(source, dest_dir)?;
        self.move_to(source, &dest)
    }

    /// Copy `source` to the exact path `dest`.
    ///
    /// # Errors
    /// See [`TransferError`].
    pub fn copy(&self, source: &Path, dest: &Path) -> Result<Transferred, TransferError> {
        let meta = Self::require(source)?;
        let Some(dest) = self.resolve(dest)? else {
            return Ok(Transferred {
                dest: dest.to_path_buf(),
                skipped: true,
            });
        };
        // Checked before any write: copying a directory into its own
        // subtree recurses until the disk is full.
        if meta.is_dir() && Self::is_within(&dest, source) {
            return Err(TransferError::WouldRecurse(source.to_path_buf(), dest));
        }
        self.copy_entry(source, &dest, &meta)?;
        Ok(Transferred {
            dest,
            skipped: false,
        })
    }

    /// Move `source` to the exact path `dest`.
    ///
    /// Tries `rename(2)` first and falls back to copy-then-delete only when
    /// the two paths are on different filesystems.
    ///
    /// # Errors
    /// See [`TransferError`].
    pub fn move_to(&self, source: &Path, dest: &Path) -> Result<Transferred, TransferError> {
        let meta = Self::require(source)?;
        let Some(dest) = self.resolve(dest)? else {
            return Ok(Transferred {
                dest: dest.to_path_buf(),
                skipped: true,
            });
        };
        if meta.is_dir() && Self::is_within(&dest, source) {
            return Err(TransferError::WouldRecurse(source.to_path_buf(), dest));
        }
        Self::ensure_parent(&dest)?;
        // An existing destination must go first: `rename` onto a non-empty
        // directory fails with ENOTEMPTY even when the policy said replace.
        if std::fs::symlink_metadata(&dest).is_ok() {
            Self::remove_any(&dest)?;
        }
        match std::fs::rename(source, &dest) {
            Ok(()) => {
                self.progress.add_file();
                Ok(Transferred {
                    dest,
                    skipped: false,
                })
            }
            // The one case where a move legitimately becomes a copy.
            Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
                self.copy_entry(source, &dest, &meta)?;
                // Only now, with the copy complete on disk, is it safe to
                // let go of the original.
                Self::remove_any(source)?;
                Ok(Transferred {
                    dest,
                    skipped: false,
                })
            }
            Err(e) => Err(TransferError::Io(source.to_path_buf(), e)),
        }
    }

    /// Permanently delete a path, recursively.
    ///
    /// Never follows symlinks: deleting a link removes the link, leaving
    /// its target untouched. The opposite would let a link inside a doomed
    /// directory take out files elsewhere on the system.
    ///
    /// # Errors
    /// See [`TransferError`].
    pub fn delete(&self, path: &Path) -> Result<(), TransferError> {
        Self::require(path)?;
        Self::remove_any(path)?;
        self.progress.add_file();
        Ok(())
    }

    /// Stat `source`, turning absence into a typed error.
    fn require(source: &Path) -> Result<std::fs::Metadata, TransferError> {
        std::fs::symlink_metadata(source).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                TransferError::NotFound(source.to_path_buf())
            } else {
                TransferError::Io(source.to_path_buf(), e)
            }
        })
    }

    /// The destination path for `source` placed inside `dest_dir`.
    fn dest_for(source: &Path, dest_dir: &Path) -> Result<PathBuf, TransferError> {
        let name = source
            .file_name()
            .ok_or_else(|| TransferError::NotFound(source.to_path_buf()))?;
        Ok(dest_dir.join(name))
    }

    /// Apply the conflict policy, mapping `Ask` to an error and `Skip` to
    /// `None`.
    fn resolve(&self, dest: &Path) -> Result<Option<PathBuf>, TransferError> {
        match self.resolver.resolve(dest) {
            Resolution::Use(p) => Ok(Some(p)),
            Resolution::Replace => Ok(Some(dest.to_path_buf())),
            Resolution::Skip => Ok(None),
            Resolution::Ask => Err(TransferError::Conflict(dest.to_path_buf())),
        }
    }

    /// Whether `inner` is `outer` or sits beneath it.
    ///
    /// Uses `canonicalize` where possible so `/tmp/a/../a/b` is recognised
    /// as being inside `/tmp/a`; falls back to the literal paths when the
    /// destination doesn't exist yet.
    fn is_within(inner: &Path, outer: &Path) -> bool {
        let outer_c = std::fs::canonicalize(outer).unwrap_or_else(|_| outer.to_path_buf());
        // The destination usually doesn't exist yet, so canonicalize its
        // nearest existing ancestor and re-attach the remainder.
        let inner_c = Self::canonical_ish(inner);
        inner_c.starts_with(&outer_c)
    }

    /// Canonicalize as much of `path` as exists, keeping the rest verbatim.
    fn canonical_ish(path: &Path) -> PathBuf {
        if let Ok(c) = std::fs::canonicalize(path) {
            return c;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => Self::canonical_ish(parent).join(name),
            _ => path.to_path_buf(),
        }
    }

    /// Create `dest`'s parent directory if it is missing.
    fn ensure_parent(dest: &Path) -> Result<(), TransferError> {
        let Some(parent) = dest.parent() else {
            return Ok(());
        };
        if parent.as_os_str().is_empty() || parent.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(parent).map_err(|e| TransferError::Io(parent.to_path_buf(), e))
    }

    /// Delete a path of any type, recursively, without following links.
    fn remove_any(path: &Path) -> Result<(), TransferError> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| TransferError::Io(path.to_path_buf(), e))?;
        // `is_dir` on `symlink_metadata` is false for a symlink *to* a
        // directory, which is exactly what we want: `remove_file` unlinks
        // the link and leaves the target alone.
        let result = if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.map_err(|e| TransferError::Io(path.to_path_buf(), e))
    }

    /// Copy one entry, dispatching on its type and recursing for
    /// directories.
    fn copy_entry(
        &self,
        source: &Path,
        dest: &Path,
        meta: &std::fs::Metadata,
    ) -> Result<(), TransferError> {
        if self.progress.is_cancelled() {
            return Err(TransferError::Cancelled);
        }
        Self::ensure_parent(dest)?;
        if meta.is_symlink() {
            return self.copy_symlink(source, dest);
        }
        if meta.is_dir() {
            return self.copy_dir(source, dest, meta);
        }
        self.copy_file(source, dest, meta)
    }

    /// Recreate a symlink verbatim.
    ///
    /// The target text is copied unchanged, so a *relative* link stays
    /// relative and keeps pointing within the copied tree.
    fn copy_symlink(&self, source: &Path, dest: &Path) -> Result<(), TransferError> {
        let target =
            std::fs::read_link(source).map_err(|e| TransferError::Io(source.to_path_buf(), e))?;
        if std::fs::symlink_metadata(dest).is_ok() {
            Self::remove_any(dest)?;
        }
        std::os::unix::fs::symlink(&target, dest)
            .map_err(|e| TransferError::Io(dest.to_path_buf(), e))?;
        self.progress.add_file();
        Ok(())
    }

    /// Copy a directory and everything under it.
    fn copy_dir(
        &self,
        source: &Path,
        dest: &Path,
        meta: &std::fs::Metadata,
    ) -> Result<(), TransferError> {
        if !dest.exists() {
            std::fs::create_dir(dest).map_err(|e| TransferError::Io(dest.to_path_buf(), e))?;
        }
        let entries =
            std::fs::read_dir(source).map_err(|e| TransferError::Io(source.to_path_buf(), e))?;
        for entry in entries {
            if self.progress.is_cancelled() {
                return Err(TransferError::Cancelled);
            }
            let entry = entry.map_err(|e| TransferError::Io(source.to_path_buf(), e))?;
            let child_meta = entry
                .metadata()
                .map_err(|e| TransferError::Io(entry.path(), e))?;
            self.copy_entry(&entry.path(), &dest.join(entry.file_name()), &child_meta)?;
        }
        // Permissions last: copying into a directory we just made read-only
        // would fail, so we relax the mode until the contents are in place.
        Self::copy_permissions(dest, meta)?;
        self.progress.add_file();
        Ok(())
    }

    /// Copy a regular file's contents, in chunks, checking for cancellation
    /// between them.
    fn copy_file(
        &self,
        source: &Path,
        dest: &Path,
        meta: &std::fs::Metadata,
    ) -> Result<(), TransferError> {
        let mut reader =
            std::fs::File::open(source).map_err(|e| TransferError::Io(source.to_path_buf(), e))?;
        let mut writer =
            std::fs::File::create(dest).map_err(|e| TransferError::Io(dest.to_path_buf(), e))?;
        // 256 KiB: large enough that syscall overhead is negligible, small
        // enough that a cancel is felt promptly even on a slow device.
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            if self.progress.is_cancelled() {
                // Drop the partial file rather than leave a truncated
                // lookalike that the user might mistake for a real copy.
                drop(writer);
                let _ = std::fs::remove_file(dest);
                return Err(TransferError::Cancelled);
            }
            let n = reader
                .read(&mut buf)
                .map_err(|e| TransferError::Io(source.to_path_buf(), e))?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .map_err(|e| TransferError::Io(dest.to_path_buf(), e))?;
            self.progress.add_bytes(n as u64);
        }
        writer
            .flush()
            .map_err(|e| TransferError::Io(dest.to_path_buf(), e))?;
        drop(writer);
        Self::copy_permissions(dest, meta)?;
        self.progress.add_file();
        Ok(())
    }

    /// Carry the source's mode across, so an executable stays executable
    /// and a private file stays private.
    fn copy_permissions(dest: &Path, meta: &std::fs::Metadata) -> Result<(), TransferError> {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(mode))
            .map_err(|e| TransferError::Io(dest.to_path_buf(), e))
    }
}

/// `EXDEV` -- "invalid cross-device link".
///
/// Hard-coded rather than pulled from a libc binding: this crate has no
/// dependencies, and the value is fixed at 18 on every Linux ABI.
const fn libc_exdev() -> i32 {
    18
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conflict::ConflictPolicy;
    use crate::test_support::TempDir;

    /// A transfer with the given policy and a fresh progress handle.
    fn transfer(policy: ConflictPolicy) -> Transfer {
        Transfer::new(ConflictResolver::new(policy), Progress::new())
    }

    #[test]
    fn copies_a_file_byte_for_byte() {
        let dir = TempDir::new("copy-file");
        let src = dir.create_file("a.txt", b"hello world");
        let out = transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("b.txt"))
            .expect("copy");
        assert!(!out.skipped);
        assert_eq!(dir.read("b.txt"), b"hello world");
        // The original must still be there.
        assert_eq!(dir.read("a.txt"), b"hello world");
    }

    #[test]
    fn copies_a_large_file_across_many_chunks() {
        // Exercises the chunk loop rather than a single short read.
        let dir = TempDir::new("copy-large");
        let data: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let src = dir.create_file("big.bin", &data);
        transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("big-copy.bin"))
            .expect("copy");
        assert_eq!(dir.read("big-copy.bin"), data);
    }

    #[test]
    fn copies_a_directory_tree() {
        let dir = TempDir::new("copy-tree");
        dir.create_file("src/one.txt", b"1");
        dir.create_file("src/sub/two.txt", b"22");
        transfer(ConflictPolicy::Ask)
            .copy(&dir.join("src"), &dir.join("dst"))
            .expect("copy");
        assert_eq!(dir.read("dst/one.txt"), b"1");
        assert_eq!(dir.read("dst/sub/two.txt"), b"22");
    }

    #[test]
    fn copying_a_symlink_recreates_the_link_not_the_target() {
        // Following would turn a 40-byte link into a full duplicate.
        let dir = TempDir::new("copy-symlink");
        dir.create_file("real.txt", b"payload");
        let link = dir.create_symlink("link.txt", "real.txt");
        transfer(ConflictPolicy::Ask)
            .copy(&link, &dir.join("copy.txt"))
            .expect("copy");
        let meta = std::fs::symlink_metadata(dir.join("copy.txt")).expect("stat");
        assert!(meta.is_symlink(), "must still be a symlink");
        assert_eq!(
            std::fs::read_link(dir.join("copy.txt")).expect("readlink"),
            Path::new("real.txt"),
            "relative target must stay relative"
        );
    }

    #[test]
    fn a_broken_symlink_copies_fine() {
        let dir = TempDir::new("copy-broken");
        let link = dir.create_symlink("link.txt", "gone");
        transfer(ConflictPolicy::Ask)
            .copy(&link, &dir.join("copy.txt"))
            .expect("copy");
        assert!(std::fs::symlink_metadata(dir.join("copy.txt")).is_ok());
    }

    #[test]
    fn an_executable_bit_survives_a_copy() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("copy-mode");
        let src = dir.create_file("run.sh", b"#!/bin/sh\n");
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("run2.sh"))
            .expect("copy");
        let mode = std::fs::metadata(dir.join("run2.sh"))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn copying_onto_an_existing_file_asks_by_default() {
        let dir = TempDir::new("copy-conflict");
        let src = dir.create_file("a.txt", b"new");
        dir.create_file("b.txt", b"old");
        let err = transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("b.txt"))
            .expect_err("should refuse");
        assert!(matches!(err, TransferError::Conflict(_)));
        // Crucially, the existing file is untouched.
        assert_eq!(dir.read("b.txt"), b"old");
    }

    #[test]
    fn skip_leaves_the_destination_untouched() {
        let dir = TempDir::new("copy-skip");
        let src = dir.create_file("a.txt", b"new");
        dir.create_file("b.txt", b"old");
        let out = transfer(ConflictPolicy::Skip)
            .copy(&src, &dir.join("b.txt"))
            .expect("skip is not an error");
        assert!(out.skipped);
        assert_eq!(dir.read("b.txt"), b"old");
    }

    #[test]
    fn rename_keeps_both_copies() {
        let dir = TempDir::new("copy-rename");
        let src = dir.create_file("a.txt", b"new");
        dir.create_file("b.txt", b"old");
        let out = transfer(ConflictPolicy::Rename)
            .copy(&src, &dir.join("b.txt"))
            .expect("copy");
        assert_eq!(out.dest, dir.join("b (1).txt"));
        assert_eq!(dir.read("b.txt"), b"old");
        assert_eq!(dir.read("b (1).txt"), b"new");
    }

    #[test]
    fn overwrite_replaces_the_destination() {
        let dir = TempDir::new("copy-overwrite");
        let src = dir.create_file("a.txt", b"new");
        dir.create_file("b.txt", b"old");
        transfer(ConflictPolicy::Overwrite)
            .copy(&src, &dir.join("b.txt"))
            .expect("copy");
        assert_eq!(dir.read("b.txt"), b"new");
    }

    #[test]
    fn copying_a_missing_source_reports_not_found() {
        let dir = TempDir::new("copy-missing");
        let err = transfer(ConflictPolicy::Ask)
            .copy(&dir.join("nope.txt"), &dir.join("out.txt"))
            .expect_err("should fail");
        assert!(matches!(err, TransferError::NotFound(_)));
    }

    #[test]
    fn copying_a_directory_into_itself_is_refused() {
        // Without the guard this recurses until the disk fills.
        let dir = TempDir::new("copy-recurse");
        let src = dir.create_dir("tree");
        dir.create_file("tree/a.txt", b"a");
        let err = transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("tree/inner"))
            .expect_err("should refuse");
        assert!(matches!(err, TransferError::WouldRecurse(_, _)));
    }

    #[test]
    fn copy_into_keeps_the_source_name() {
        let dir = TempDir::new("copy-into");
        let src = dir.create_file("photo.png", b"img");
        let target = dir.create_dir("album");
        let out = transfer(ConflictPolicy::Ask)
            .copy_into(&src, &target)
            .expect("copy");
        assert_eq!(out.dest, target.join("photo.png"));
        assert_eq!(dir.read("album/photo.png"), b"img");
    }

    #[test]
    fn moving_within_a_filesystem_removes_the_source() {
        let dir = TempDir::new("move-file");
        let src = dir.create_file("a.txt", b"data");
        transfer(ConflictPolicy::Ask)
            .move_to(&src, &dir.join("b.txt"))
            .expect("move");
        assert_eq!(dir.read("b.txt"), b"data");
        assert!(!src.exists(), "source must be gone after a move");
    }

    #[test]
    fn moving_preserves_inode_identity_within_a_filesystem() {
        // Proves it was a rename, not a copy-then-delete: a copy would
        // allocate a new inode and break every hard link to the file.
        use std::os::unix::fs::MetadataExt;
        let dir = TempDir::new("move-inode");
        let src = dir.create_file("a.txt", b"data");
        let before = std::fs::metadata(&src).expect("stat").ino();
        transfer(ConflictPolicy::Ask)
            .move_to(&src, &dir.join("b.txt"))
            .expect("move");
        let after = std::fs::metadata(dir.join("b.txt")).expect("stat").ino();
        assert_eq!(before, after);
    }

    #[test]
    fn moving_a_tree_moves_everything() {
        let dir = TempDir::new("move-tree");
        dir.create_file("src/a.txt", b"1");
        dir.create_file("src/sub/b.txt", b"2");
        transfer(ConflictPolicy::Ask)
            .move_to(&dir.join("src"), &dir.join("dst"))
            .expect("move");
        assert_eq!(dir.read("dst/sub/b.txt"), b"2");
        assert!(!dir.join("src").exists());
    }

    #[test]
    fn moving_onto_an_existing_directory_replaces_it_under_overwrite() {
        // `rename` onto a non-empty directory is ENOTEMPTY; the engine must
        // clear the destination first.
        let dir = TempDir::new("move-onto-dir");
        dir.create_file("src/new.txt", b"new");
        dir.create_file("dst/old.txt", b"old");
        transfer(ConflictPolicy::Overwrite)
            .move_to(&dir.join("src"), &dir.join("dst"))
            .expect("move");
        assert_eq!(dir.read("dst/new.txt"), b"new");
        assert!(!dir.join("dst/old.txt").exists());
    }

    #[test]
    fn a_refused_move_leaves_the_source_in_place() {
        // The most important guarantee in the crate: a failure never
        // destroys the original.
        let dir = TempDir::new("move-refused");
        let src = dir.create_file("a.txt", b"precious");
        dir.create_file("b.txt", b"old");
        let err = transfer(ConflictPolicy::Ask)
            .move_to(&src, &dir.join("b.txt"))
            .expect_err("should refuse");
        assert!(matches!(err, TransferError::Conflict(_)));
        assert_eq!(dir.read("a.txt"), b"precious");
        assert_eq!(dir.read("b.txt"), b"old");
    }

    #[test]
    fn moving_a_symlink_moves_the_link_not_the_target() {
        let dir = TempDir::new("move-symlink");
        dir.create_file("real.txt", b"payload");
        let link = dir.create_symlink("link.txt", "real.txt");
        transfer(ConflictPolicy::Ask)
            .move_to(&link, &dir.join("moved.txt"))
            .expect("move");
        assert!(
            std::fs::symlink_metadata(dir.join("moved.txt"))
                .expect("stat")
                .is_symlink()
        );
        // The target must be entirely unaffected.
        assert_eq!(dir.read("real.txt"), b"payload");
    }

    #[test]
    fn deleting_a_symlink_spares_its_target() {
        // The classic data-loss bug: following the link would delete the
        // real file somewhere else on disk.
        let dir = TempDir::new("delete-symlink");
        dir.create_file("real.txt", b"precious");
        let link = dir.create_symlink("link.txt", "real.txt");
        transfer(ConflictPolicy::Ask).delete(&link).expect("delete");
        assert!(!link.exists());
        assert_eq!(dir.read("real.txt"), b"precious");
    }

    #[test]
    fn deleting_a_tree_with_an_outward_symlink_spares_the_target() {
        // A link inside a doomed directory must not take out files that
        // live outside it.
        let dir = TempDir::new("delete-outward");
        dir.create_file("keep/precious.txt", b"precious");
        dir.create_dir("doomed");
        dir.create_symlink("doomed/escape", "../keep");
        transfer(ConflictPolicy::Ask)
            .delete(&dir.join("doomed"))
            .expect("delete");
        assert!(!dir.join("doomed").exists());
        assert_eq!(dir.read("keep/precious.txt"), b"precious");
    }

    #[test]
    fn deleting_a_missing_path_reports_not_found() {
        let dir = TempDir::new("delete-missing");
        let err = transfer(ConflictPolicy::Ask)
            .delete(&dir.join("nope"))
            .expect_err("should fail");
        assert!(matches!(err, TransferError::NotFound(_)));
    }

    #[test]
    fn progress_accumulates_over_a_copy() {
        let dir = TempDir::new("copy-progress");
        dir.create_file("src/a.txt", b"12345");
        dir.create_file("src/b.txt", b"678");
        let progress = Progress::new();
        let t = Transfer::new(ConflictResolver::new(ConflictPolicy::Ask), progress.clone());
        t.copy(&dir.join("src"), &dir.join("dst")).expect("copy");
        let snap = progress.snapshot();
        assert_eq!(snap.bytes_done, 8);
        // two files plus the directory itself
        assert_eq!(snap.files_done, 3);
    }

    #[test]
    fn a_cancelled_copy_stops_and_leaves_no_partial_file() {
        // A truncated file that looks complete is worse than no file.
        let dir = TempDir::new("copy-cancelled");
        let data = vec![7u8; 900_000];
        let src = dir.create_file("big.bin", &data);
        let progress = Progress::new();
        progress.cancel();
        let t = Transfer::new(ConflictResolver::new(ConflictPolicy::Ask), progress);
        let err = t
            .copy(&src, &dir.join("out.bin"))
            .expect_err("should cancel");
        assert!(matches!(err, TransferError::Cancelled));
        assert!(!dir.join("out.bin").exists(), "no partial file left behind");
        // And the source is of course untouched.
        assert_eq!(dir.read("big.bin").len(), 900_000);
    }

    #[test]
    fn a_missing_destination_parent_is_created() {
        let dir = TempDir::new("copy-mkparent");
        let src = dir.create_file("a.txt", b"x");
        transfer(ConflictPolicy::Ask)
            .copy(&src, &dir.join("deep/nested/a.txt"))
            .expect("copy");
        assert_eq!(dir.read("deep/nested/a.txt"), b"x");
    }
}
