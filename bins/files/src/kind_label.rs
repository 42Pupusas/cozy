//! [`KindLabel`] -- the `kind` column's human words.
//!
//! The design guide's mockup shows `directory`, `pdf`, `mpeg-4`, `markdown`,
//! `archive` -- what a person would *call* the file, not its media type.
//! Printing `application/vnd.oasis.opendocument.text` in a 17%-wide column
//! would be both unreadable and useless.
//!
//! The mapping is deliberately shallow. A full media-type-to-prose table is
//! thousands of entries and belongs to a desktop's shared-mime-info database;
//! what a file list needs is a short word for the couple of dozen types a
//! person actually meets, and a sane derivation for everything else.
//!
//! Derivation order, most specific first:
//!
//! 1. Structural kinds the filesystem knows (directory, symlink) -- these
//!    outrank content type, because "it is a link" is the fact that changes
//!    what every operation does.
//! 2. A curated table of exact media types with well-known names.
//! 3. The subtype with its vendor/structural noise stripped (`x-`, `vnd.`,
//!    `+xml`), which turns `application/x-tar` into `tar` for free.
//! 4. The top-level type (`image`, `video`) as a last resort.

use fsmodel::entry::Entry;
use fsmodel::kind::EntryKind;
use mime::MimeType;

/// Names entry kinds for display.
#[derive(Debug, Clone, Copy, Default)]
pub struct KindLabel;

impl KindLabel {
    /// Shown when nothing better can be derived.
    ///
    /// A word rather than a blank, so the column never looks like it failed
    /// to render.
    pub const UNKNOWN: &'static str = "file";

    /// Exact media types worth naming by hand, because the derived form
    /// would be noise (`vnd.openxmlformats-officedocument...`) or plain
    /// wrong (`plain` for a text file).
    const NAMED: [(&'static str, &'static str); 24] = [
        ("inode/directory", "directory"),
        ("application/pdf", "pdf"),
        ("text/plain", "text"),
        ("text/markdown", "markdown"),
        ("text/html", "html"),
        ("text/csv", "csv"),
        ("application/json", "json"),
        ("application/xml", "xml"),
        ("application/zip", "archive"),
        ("application/gzip", "archive"),
        ("application/x-tar", "archive"),
        ("application/x-bzip2", "archive"),
        ("application/x-xz", "archive"),
        ("application/zstd", "archive"),
        ("application/x-7z-compressed", "archive"),
        ("application/vnd.rar", "archive"),
        ("video/mp4", "mpeg-4"),
        ("video/x-matroska", "matroska"),
        ("audio/mpeg", "mp3"),
        ("audio/flac", "flac"),
        ("image/jpeg", "jpeg"),
        ("image/svg+xml", "svg"),
        ("application/x-shellscript", "script"),
        ("application/octet-stream", "file"),
    ];

    /// The word for an entry, given its already-detected media type.
    ///
    /// `mime` is optional because detection is a database lookup the painter
    /// may not have done for an off-screen row; a missing type degrades to
    /// the structural answer rather than forcing the caller to detect.
    #[must_use]
    pub fn of(entry: &Entry, mime: Option<&MimeType>) -> String {
        // Structure first: a symlink is a symlink whatever it points at, and
        // that is the fact that changes what activating it does.
        if entry.kind == EntryKind::Symlink {
            return if entry.is_broken_symlink() {
                "broken link".to_owned()
            } else if entry.target_is_dir {
                "link to dir".to_owned()
            } else {
                "link".to_owned()
            };
        }
        if entry.kind.is_dir() {
            return "directory".to_owned();
        }
        mime.map_or_else(|| Self::UNKNOWN.to_owned(), Self::describe)
    }

    /// The word for a media type on its own.
    #[must_use]
    pub fn describe(mime: &MimeType) -> String {
        if let Some((_, name)) = Self::NAMED.iter().find(|(t, _)| *t == mime.as_str()) {
            return (*name).to_owned();
        }
        let derived = Self::strip_noise(mime.subtype());
        if derived.is_empty() {
            // A type like `image/` should be impossible (`MimeType::parse`
            // rejects it), but falling back to the top level is still more
            // useful than an empty cell.
            let top = mime.top_level();
            return if top.is_empty() {
                Self::UNKNOWN.to_owned()
            } else {
                top.to_owned()
            };
        }
        derived
    }

    /// Strip the conventional noise off a subtype.
    ///
    /// `x-` marks an unregistered type, `vnd.` a vendor-specific one, and
    /// `+xml`/`+json` are RFC 6839 structured suffixes. None of those three
    /// tell a person anything, and all three are pure prefix/suffix work --
    /// so removing them turns most types into their own good label.
    fn strip_noise(subtype: &str) -> String {
        let mut s = subtype;
        for prefix in ["x-", "vnd."] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest;
            }
        }
        for suffix in ["+xml", "+json", "+zip"] {
            if let Some(rest) = s.strip_suffix(suffix) {
                s = rest;
            }
        }
        // Vendor types often carry a further dotted path
        // (`vnd.oasis.opendocument.text`); the last segment is the useful
        // word, and the rest is the organisation that registered it.
        s.rsplit('.').next().unwrap_or(s).to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::KindLabel;
    use crate::test_support::TempDir;
    use fsmodel::entry::Entry;
    use mime::MimeType;

    fn mime(raw: &str) -> MimeType {
        MimeType::parse(raw).expect("a well-formed type")
    }

    /// The guide's own mockup strings must come out of the curated table.
    #[test]
    fn the_guide_examples_get_their_expected_words() {
        assert_eq!(KindLabel::describe(&mime("application/pdf")), "pdf");
        assert_eq!(KindLabel::describe(&mime("video/mp4")), "mpeg-4");
        assert_eq!(KindLabel::describe(&mime("text/markdown")), "markdown");
        assert_eq!(KindLabel::describe(&mime("application/gzip")), "archive");
    }

    /// Every flavour of archive should read as one word, so a directory of
    /// mixed tarballs scans as a group rather than as five unrelated types.
    #[test]
    fn archives_all_read_the_same() {
        for t in [
            "application/zip",
            "application/gzip",
            "application/x-tar",
            "application/x-xz",
            "application/zstd",
            "application/x-7z-compressed",
        ] {
            assert_eq!(KindLabel::describe(&mime(t)), "archive", "for {t}");
        }
    }

    /// An unlisted type must still produce a readable word rather than the
    /// raw media type -- this is the case the curated table cannot cover.
    #[test]
    fn an_unlisted_type_derives_a_readable_word() {
        assert_eq!(KindLabel::describe(&mime("image/png")), "png");
        assert_eq!(KindLabel::describe(&mime("audio/ogg")), "ogg");
        assert_eq!(KindLabel::describe(&mime("font/woff2")), "woff2");
    }

    /// The `x-` unregistered marker is noise to a reader.
    #[test]
    fn the_unregistered_prefix_is_stripped() {
        assert_eq!(KindLabel::describe(&mime("application/x-rust")), "rust");
        assert_eq!(KindLabel::describe(&mime("text/x-python")), "python");
    }

    /// Vendor types must reduce to their last meaningful segment, not print
    /// a 60-character registration path in a 17%-wide column.
    #[test]
    fn a_vendor_type_reduces_to_its_last_segment() {
        assert_eq!(
            KindLabel::describe(&mime("application/vnd.oasis.opendocument.text")),
            "text"
        );
        assert_eq!(
            KindLabel::describe(&mime(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            )),
            "document"
        );
    }

    /// RFC 6839 structured suffixes are noise too -- except where the
    /// curated table has a better word, which `svg+xml` does.
    #[test]
    fn structured_suffixes_are_stripped() {
        assert_eq!(KindLabel::describe(&mime("application/atom+xml")), "atom");
        assert_eq!(KindLabel::describe(&mime("application/ld+json")), "ld");
        assert_eq!(
            KindLabel::describe(&mime("image/svg+xml")),
            "svg",
            "the curated word must win over the derived one"
        );
    }

    /// No label may ever be empty: an empty cell reads as a rendering bug.
    #[test]
    fn no_type_ever_produces_an_empty_label() {
        for t in [
            "application/octet-stream",
            "application/x-",
            "text/vnd.",
            "a/b",
            "application/vnd.x-.",
        ] {
            let Some(parsed) = MimeType::parse(t) else {
                continue;
            };
            assert!(
                !KindLabel::describe(&parsed).is_empty(),
                "{t} produced an empty label"
            );
        }
    }

    /// A directory is named structurally, without consulting a media type at
    /// all -- the filesystem already knows.
    #[test]
    fn a_directory_is_named_from_its_kind() {
        let tmp = TempDir::new("kind-dir");
        tmp.create_dir("sub");
        let entry = Entry::from_path(&tmp.path().join("sub")).expect("entry");
        assert_eq!(KindLabel::of(&entry, None), "directory");
    }

    /// Being a link outranks whatever the link points at: that is the fact
    /// that changes what every operation on it does.
    #[test]
    fn a_symlink_is_named_as_a_link_not_as_its_target() {
        let tmp = TempDir::new("kind-link");
        tmp.create_file("target.txt", b"x");
        tmp.create_symlink("link", "target.txt");
        let entry = Entry::from_path(&tmp.path().join("link")).expect("entry");
        assert_eq!(
            KindLabel::of(&entry, Some(&mime("text/plain"))),
            "link",
            "the media type must not override the structural fact"
        );
    }

    #[test]
    fn a_link_to_a_directory_says_so() {
        let tmp = TempDir::new("kind-link-dir");
        tmp.create_dir("real");
        tmp.create_symlink("link", "real");
        let entry = Entry::from_path(&tmp.path().join("link")).expect("entry");
        assert_eq!(KindLabel::of(&entry, None), "link to dir");
    }

    /// A broken link is worth calling out: it looks normal but every
    /// operation on it will behave surprisingly.
    #[test]
    fn a_broken_link_is_called_out() {
        let tmp = TempDir::new("kind-broken");
        tmp.create_symlink("dangling", "nowhere");
        let entry = Entry::from_path(&tmp.path().join("dangling")).expect("entry");
        assert_eq!(KindLabel::of(&entry, None), "broken link");
    }

    /// A plain file with no detected type must still get a word.
    #[test]
    fn an_undetected_file_falls_back_to_a_word() {
        let tmp = TempDir::new("kind-plain");
        tmp.create_file("mystery", b"x");
        let entry = Entry::from_path(&tmp.path().join("mystery")).expect("entry");
        assert_eq!(KindLabel::of(&entry, None), KindLabel::UNKNOWN);
        assert!(!KindLabel::UNKNOWN.is_empty());
    }

    /// Labels stay short enough for a narrow column; a 17% column at a
    /// typical width is around twenty characters.
    #[test]
    fn every_curated_label_is_short_enough_to_fit() {
        for (media, label) in KindLabel::NAMED {
            assert!(
                label.len() <= 12,
                "{media} -> {label} is too long for the column"
            );
        }
    }
}
