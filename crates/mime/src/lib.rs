//! Media-type detection: what *is* this file?
//!
//! Answers the question in the order the shared-mime-info spec recommends,
//! which is also the order of decreasing cheapness:
//!
//! 1. **Is it a directory?** Free -- we already know from the entry.
//! 2. **Filename glob** against the system `globs2` database. One string
//!    comparison for the common case, no I/O.
//! 3. **Content sniffing** of the first 512 bytes, only when the name told
//!    us nothing.
//!
//! That ordering matters for a file browser: step 2 costs nothing and
//! resolves the vast majority of entries, so a directory of a thousand
//! files does not become a thousand `open()` calls. Sniffing is reserved
//! for the genuinely ambiguous ones (`README`, `LICENSE`, a bare `blob`).
//!
//! Contains no Wayland or rendering knowledge, matching `crates/fsmodel`
//! and `crates/launcher` -- every rule here is unit-testable headlessly.

pub mod associations;
pub mod glob;
pub mod globs;
pub mod mime_type;
pub mod sniff;

pub use associations::{Associations, DesktopId};
pub use globs::GlobDatabase;
pub use mime_type::MimeType;
pub use sniff::Sniffer;

use std::path::Path;

/// How a type was determined -- useful for UI ("detected by content") and
/// for tests that need to assert *which* mechanism fired, not just the
/// answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    /// The path is a directory.
    Directory,
    /// Matched a filename pattern in the glob database.
    Glob,
    /// Identified from the file's leading bytes.
    Content,
    /// Nothing matched; this is the `application/octet-stream` fallback.
    Fallback,
}

/// A resolved type plus how we got there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub mime: MimeType,
    pub detection: Detection,
}

/// The combined type-detection database.
///
/// Loads the system glob database once at construction and reuses it for
/// every query -- re-reading ~1400 lines per file would be absurd, and a
/// browser resolves types for a whole directory at a time.
#[derive(Debug, Clone, Default)]
pub struct MimeDb {
    globs: GlobDatabase,
    sniffer: Sniffer,
}

impl MimeDb {
    /// Load the system glob database.
    ///
    /// Never fails: a machine with no shared-mime-info installed simply
    /// falls back to content sniffing more often.
    #[must_use]
    pub fn load() -> Self {
        Self {
            globs: GlobDatabase::load(),
            sniffer: Sniffer::new(),
        }
    }

    /// Build from an explicit `globs2` document -- the seam tests use to
    /// avoid depending on what's installed.
    #[must_use]
    pub fn from_globs_data(text: &str) -> Self {
        Self {
            globs: GlobDatabase::from_str_data(text),
            sniffer: Sniffer::new(),
        }
    }

    /// The underlying glob database.
    #[must_use]
    pub const fn globs(&self) -> &GlobDatabase {
        &self.globs
    }

    /// Determine the type of `path`, reading from disk only if the name is
    /// inconclusive.
    #[must_use]
    pub fn detect(&self, path: &Path) -> Detected {
        if path.is_dir() {
            return Detected {
                mime: MimeType::directory(),
                detection: Detection::Directory,
            };
        }

        if let Some(mime) = self.globs.lookup_path(path) {
            return Detected {
                mime,
                detection: Detection::Glob,
            };
        }

        // Only now do we touch the file's contents.
        self.sniffer.sniff_path(path).map_or_else(
            || Detected {
                mime: MimeType::octet_stream(),
                detection: Detection::Fallback,
            },
            |mime| Detected {
                mime,
                detection: Detection::Content,
            },
        )
    }

    /// Determine a type from a file name alone, with no I/O at all.
    ///
    /// For rendering a directory listing at speed: an icon only needs the
    /// glob answer, and a browser must not stat-and-read every visible row
    /// while the user scrolls.
    #[must_use]
    pub fn detect_by_name(&self, filename: &str) -> Option<MimeType> {
        self.globs.lookup(filename)
    }

    /// Determine a type from bytes already in hand.
    #[must_use]
    pub fn detect_bytes(&self, data: &[u8]) -> MimeType {
        self.sniffer.sniff(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOBS: &str = "\
80:text/html:*.html
60:text/x-python:*.py
50:image/png:*.png
";

    /// Scratch directory helper -- same rationale as `crates/fsmodel`'s:
    /// these rules are about real files, so mocking would test the mock.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mime-test-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }

        fn file(&self, name: &str, contents: &[u8]) -> std::path::PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, contents).expect("write");
            p
        }

        fn dir(&self, name: &str) -> std::path::PathBuf {
            let p = self.0.join(name);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directories_are_detected_without_touching_contents() {
        let scratch = Scratch::new("dir");
        let db = MimeDb::from_globs_data(GLOBS);
        let got = db.detect(&scratch.dir("subdir"));
        assert_eq!(got.detection, Detection::Directory);
        assert!(got.mime.is_directory());
    }

    /// The name must win when it's conclusive -- this is what keeps a
    /// large directory listing free of per-file reads.
    #[test]
    fn extension_wins_without_reading_the_file() {
        let scratch = Scratch::new("glob");
        let db = MimeDb::from_globs_data(GLOBS);
        // Contents deliberately contradict the extension; the glob still wins.
        let path = scratch.file("page.html", b"\x89PNG\r\n\x1a\n");
        let got = db.detect(&path);
        assert_eq!(got.detection, Detection::Glob);
        assert_eq!(got.mime.to_string(), "text/html");
    }

    /// The case that justifies the sniffer's existence: no extension at all.
    #[test]
    fn extensionless_files_fall_through_to_content() {
        let scratch = Scratch::new("content");
        let db = MimeDb::from_globs_data(GLOBS);
        let path = scratch.file("README", b"Just some documentation.\n");
        let got = db.detect(&path);
        assert_eq!(got.detection, Detection::Content);
        assert_eq!(got.mime.to_string(), "text/plain");
        assert!(got.mime.is_text());
    }

    #[test]
    fn extensionless_binary_is_identified_by_signature() {
        let scratch = Scratch::new("content_bin");
        let db = MimeDb::from_globs_data(GLOBS);
        let path = scratch.file("mystery", b"%PDF-1.7\nbody");
        let got = db.detect(&path);
        assert_eq!(got.detection, Detection::Content);
        assert_eq!(got.mime.to_string(), "application/pdf");
    }

    #[test]
    fn unreadable_path_falls_back_to_octet_stream() {
        let db = MimeDb::from_globs_data(GLOBS);
        let got = db.detect(Path::new("/nonexistent/nope"));
        assert_eq!(got.detection, Detection::Fallback);
        assert!(got.mime.is_unknown());
    }

    #[test]
    fn name_only_detection_does_no_io() {
        let db = MimeDb::from_globs_data(GLOBS);
        // The path need not exist -- proving no filesystem access happens.
        assert_eq!(
            db.detect_by_name("script.py").map(|m| m.to_string()),
            Some("text/x-python".to_owned())
        );
        assert!(db.detect_by_name("mystery").is_none());
    }

    #[test]
    fn byte_detection_is_exposed() {
        let db = MimeDb::from_globs_data(GLOBS);
        assert_eq!(db.detect_bytes(b"GIF89a").to_string(), "image/gif");
    }

    /// End-to-end against whatever is really installed, skipped when the
    /// system database is absent so minimal containers stay green.
    #[test]
    fn works_against_the_real_system_database() {
        let db = MimeDb::load();
        if db.globs().is_empty() {
            return;
        }
        assert_eq!(
            db.detect_by_name("photo.png").map(|m| m.to_string()),
            Some("image/png".to_owned())
        );
    }
}
