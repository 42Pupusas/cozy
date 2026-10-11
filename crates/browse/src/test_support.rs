//! A scratch-directory helper for this binary's tests.
//!
//! Duplicated from `crates/fsmodel`'s own helper rather than shared, because
//! that one is `#![cfg(test)]` and so exists only inside its crate's test
//! build -- exporting it would put test scaffolding in the public API of a
//! library shipped to real consumers. Twenty lines of duplication is the
//! cheaper trade.
//!
//! Real directories, not a mock: the whole point of these tests is what the
//! browser does against actual inodes, and a mocked filesystem would only
//! test the mock.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Makes each directory unique even when two tests share a label and run in
/// the same millisecond -- the test harness is parallel by default.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temporary directory, removed recursively on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "files-test-{label}-{}-{unique}",
            std::process::id()
        ));
        // Clear any leftover from an aborted run, so a stale directory cannot
        // make a fresh test see phantom entries.
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_dir(&self, rel: &str) -> PathBuf {
        let full = self.path.join(rel);
        std::fs::create_dir_all(&full).expect("create dir");
        full
    }

    pub fn create_file(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, contents).expect("write file");
        full
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best-effort: a failure here must not mask the test's own result.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
