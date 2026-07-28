//! A minimal scratch-directory helper for this crate's tests.
//!
//! Exists because every meaningful test here has to touch a *real*
//! filesystem: `EntryKind` is derived from `std::fs::FileType`, which cannot
//! be constructed synthetically, and the whole point of `DirReader` is what
//! it does against actual inodes. Mocking the filesystem would test the mock.
//!
//! Deliberately hand-rolled rather than pulling in a `tempfile`-style
//! dependency: this workspace's crates carry no external deps, and the
//! requirement here is tiny -- make a uniquely-named directory under the
//! system temp dir, and recursively delete it on drop.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter making each `TempDir` in a test binary unique even when
/// two tests pass the same label and run in the same millisecond on
/// different threads (the default test harness is parallel).
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temporary directory, removed recursively on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a scratch directory tagged with `label` (which just aids
    /// debugging when a test leaves one behind after a panic-abort).
    pub fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fsmodel-test-{label}-{}-{unique}",
            std::process::id()
        ));
        // Clear any leftover from a previously-aborted run so a stale
        // directory can't make a fresh test see phantom entries.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// The scratch directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a subdirectory (including intermediate components).
    pub fn create_dir(&self, rel: &str) -> PathBuf {
        let full = self.path.join(rel);
        std::fs::create_dir_all(&full).expect("create dir");
        full
    }

    /// Create a file with exactly `contents`, so tests can assert on size.
    pub fn create_file(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, contents).expect("write file");
        full
    }

    /// Create a symlink at `rel` pointing to `target`.
    ///
    /// `target` is used verbatim, so tests can create *relative* links (the
    /// common real-world case) and deliberately *broken* ones -- both of
    /// which the reader has to survive.
    pub fn create_symlink(&self, rel: &str, target: &str) -> PathBuf {
        let full = self.path.join(rel);
        std::os::unix::fs::symlink(target, &full).expect("create symlink");
        full
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a failure here must not mask the test's own result,
        // and a leaked temp dir is harmless (the next run with the same
        // label clears it, and the OS reaps /tmp anyway).
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
