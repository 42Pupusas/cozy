//! Trimmed-string reads from a `/proc`/`/sys` file -- shared by every module
//! that scrapes a pseudo-file (battery, temperature, brightness, ...).
//!
//! An extension trait on [`Path`] rather than a bare `fn(path) -> Option<String>`:
//! per the crate-wide rule against free functions, reusable logic that isn't
//! obviously "owned" by one of our own structs belongs on a trait over the
//! primitive/std type it operates on instead of floating at module scope.

use std::path::Path;

/// Read a `/proc`/`/sys` pseudo-file and trim the trailing newline these
/// always have -- `None` on any I/O error (missing file, permission,
/// racing device removal), never a panic.
pub trait ReadTrimmed {
    fn read_trim(&self) -> Option<String>;
}

impl ReadTrimmed for Path {
    fn read_trim(&self) -> Option<String> {
        std::fs::read_to_string(self)
            .ok()
            .map(|s| s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Drop`-cleaned scratch file in the OS temp dir, unique per test run
    /// via the process id + a counter -- avoids a `tempfile` dependency for
    /// what's otherwise two lines of coverage.
    struct ScratchFile(std::path::PathBuf);

    impl ScratchFile {
        fn with_contents(contents: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "statusbar-read-trim-test-{}-{n}",
                std::process::id()
            ));
            std::fs::write(&path, contents).expect("write scratch file");
            Self(path)
        }
    }

    impl Drop for ScratchFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn trims_trailing_newline() {
        let f = ScratchFile::with_contents("42\n");
        assert_eq!(f.0.as_path().read_trim(), Some("42".to_string()));
    }

    #[test]
    fn missing_file_is_none() {
        assert_eq!(Path::new("/no/such/path/at/all").read_trim(), None);
    }
}
