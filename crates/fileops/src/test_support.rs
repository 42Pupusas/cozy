//! A scratch-directory helper for this crate's tests.
//!
//! Mirrors `fsmodel::test_support` rather than sharing it: making that module
//! public purely for another crate's tests would widen `fsmodel`'s API for a
//! test-only concern, and this version needs extras `fsmodel` doesn't (an
//! isolated fake `$XDG_DATA_HOME` trash root, permission twiddling).
//!
//! Everything here touches a *real* filesystem on purpose. The whole point of
//! this crate is what it does to actual inodes -- hard links, symlinks,
//! permissions, cross-directory renames. A mocked filesystem would test the
//! mock.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter making each `TempDir` unique even when two tests pass
/// the same label in the same millisecond on different threads (the default
/// test harness is parallel).
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temporary directory, removed recursively on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a scratch directory tagged with `label` (which aids debugging
    /// when a test leaves one behind after a panic-abort).
    pub fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fileops-test-{label}-{}-{unique}",
            std::process::id()
        ));
        // Clear any leftover from a previously-aborted run so a stale
        // directory can't make a fresh test see phantom entries.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// The scratch directory itself. `copy`/`move` tests will want this for
    /// whole-tree assertions; `#[allow]`d rather than deleted because
    /// removing and re-adding it churns the file for no reason.
    #[allow(dead_code, reason = "used by upcoming copy/move tests")]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to `rel` inside this directory, without creating anything.
    pub fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }

    /// Create a subdirectory (including intermediate components).
    pub fn create_dir(&self, rel: &str) -> PathBuf {
        let full = self.path.join(rel);
        std::fs::create_dir_all(&full).expect("create dir");
        full
    }

    /// Create a file with exactly `contents`, so tests can assert on size
    /// and on byte-for-byte preservation after a copy.
    pub fn create_file(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, contents).expect("write file");
        full
    }

    /// Create a symlink at `rel` pointing to `target`, verbatim -- so tests
    /// can build *relative* links (the common real-world case) and
    /// deliberately *broken* ones.
    pub fn create_symlink(&self, rel: &str, target: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::os::unix::fs::symlink(target, &full).expect("create symlink");
        full
    }

    /// Read a file back as bytes.
    pub fn read(&self, rel: &str) -> Vec<u8> {
        std::fs::read(self.path.join(rel)).expect("read file")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a failure here must not mask the test's own result,
        // and a leaked temp dir is harmless (the next run with the same
        // label clears it, and the OS reaps /tmp anyway).
        //
        // Restore permissions first: a test that made a directory
        // unreadable/unwritable to exercise an error path would otherwise
        // leave a tree that cannot be deleted.
        Self::make_removable(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl TempDir {
    /// Recursively restore `rwx` on directories so cleanup can always
    /// succeed, even after a permission-denied test.
    fn make_removable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            return;
        }
        if meta.is_dir() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    Self::make_removable(&entry.path());
                }
            }
        }
    }
}
