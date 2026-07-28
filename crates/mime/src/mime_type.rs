//! A parsed `type/subtype` media type.
//!
//! A newtype rather than a bare `String` because a MIME type has structure
//! the rest of this crate keeps asking about -- the top-level type picks the
//! icon, and the "is this text?" question drives whether we offer to open
//! something in an editor. Answering those by `split('/')`-ing a string at
//! each call site is exactly the sort of scattered logic that drifts.

/// A media type such as `text/plain` or `image/png`.
///
/// Stored lowercased and trimmed: the spec says media types are
/// case-insensitive, and normalising once at construction means every
/// comparison downstream can be a plain `==` instead of a case-folding
/// helper everyone has to remember to call.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MimeType {
    full: String,
    /// Byte offset of the `/` separating type from subtype. Cached so
    /// `top_level`/`subtype` are pure slicing rather than a repeated scan.
    slash: usize,
}

impl MimeType {
    /// The generic fallback for "we have no idea what this is", as defined
    /// by the shared-mime-info spec.
    pub const OCTET_STREAM: &'static str = "application/octet-stream";

    /// The spec's type for a plain, unremarkable directory.
    pub const DIRECTORY: &'static str = "inode/directory";

    /// Parse `raw` into a media type, returning `None` if it isn't shaped
    /// like `type/subtype`.
    ///
    /// Rejects empty halves and anything with more than one `/`, so a
    /// malformed line in a system database can't produce a `MimeType` that
    /// misbehaves when sliced later.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        let slash = trimmed.find('/')?;
        // Both halves must be non-empty, and there must be exactly one
        // separator -- `text/`, `/plain` and `a/b/c` are all malformed.
        if slash == 0 || slash + 1 >= trimmed.len() || trimmed[slash + 1..].contains('/') {
            return None;
        }
        Some(Self {
            full: trimmed.to_lowercase(),
            slash,
        })
    }

    /// The `application/octet-stream` fallback as a value.
    #[must_use]
    pub fn octet_stream() -> Self {
        Self::parse(Self::OCTET_STREAM).expect("the octet-stream constant is well-formed")
    }

    /// The `inode/directory` type as a value.
    #[must_use]
    pub fn directory() -> Self {
        Self::parse(Self::DIRECTORY).expect("the directory constant is well-formed")
    }

    /// The whole `type/subtype` string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.full
    }

    /// The part before the `/` -- `text`, `image`, `video`, …
    ///
    /// This is what an icon lookup keys off when it has no specific icon
    /// for the exact subtype.
    #[must_use]
    pub fn top_level(&self) -> &str {
        &self.full[..self.slash]
    }

    /// The part after the `/`.
    #[must_use]
    pub fn subtype(&self) -> &str {
        &self.full[self.slash + 1..]
    }

    /// Whether this is human-readable text.
    ///
    /// Covers both the obvious `text/*` and the many `application/*` types
    /// that are really text underneath (`application/json`,
    /// `application/xml`, `application/x-shellscript`, and the `+xml`/
    /// `+json` structured-suffix conventions from RFC 6839). A file browser
    /// uses this to decide whether "open in editor" is a sensible offer.
    #[must_use]
    pub fn is_text(&self) -> bool {
        if self.top_level() == "text" {
            return true;
        }
        let sub = self.subtype();
        sub.ends_with("+xml")
            || sub.ends_with("+json")
            || matches!(
                self.full.as_str(),
                "application/json"
                    | "application/xml"
                    | "application/javascript"
                    | "application/x-shellscript"
                    | "application/x-perl"
                    | "application/x-python"
                    | "application/x-ruby"
                    | "application/sql"
                    | "application/toml"
                    | "application/yaml"
            )
    }

    /// Whether this is the "unknown data" fallback.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.full == Self::OCTET_STREAM
    }

    /// Whether this names a directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.full == Self::DIRECTORY
    }
}

impl std::fmt::Display for MimeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_type_and_subtype() {
        let m = MimeType::parse("text/plain").expect("valid");
        assert_eq!(m.top_level(), "text");
        assert_eq!(m.subtype(), "plain");
        assert_eq!(m.as_str(), "text/plain");
    }

    /// Media types are case-insensitive per RFC 2045, so normalising at
    /// construction lets everything downstream compare with `==`.
    #[test]
    fn normalises_case_and_whitespace() {
        let m = MimeType::parse("  TEXT/HTML \n").expect("valid");
        assert_eq!(m.as_str(), "text/html");
        assert_eq!(m, MimeType::parse("text/html").expect("valid"));
    }

    #[test]
    fn rejects_malformed_types() {
        for bad in ["", "text", "text/", "/plain", "a/b/c", "   "] {
            assert!(
                MimeType::parse(bad).is_none(),
                "{bad:?} must not parse into a MimeType"
            );
        }
    }

    #[test]
    fn subtype_with_plus_suffix_is_intact() {
        let m = MimeType::parse("image/svg+xml").expect("valid");
        assert_eq!(m.top_level(), "image");
        assert_eq!(m.subtype(), "svg+xml");
    }

    #[test]
    fn recognises_text_like_types() {
        for text in [
            "text/plain",
            "text/x-rust",
            "application/json",
            "application/xml",
            "image/svg+xml",
            "application/x-shellscript",
            "application/ld+json",
        ] {
            assert!(
                MimeType::parse(text).expect("valid").is_text(),
                "{text} should be text-like"
            );
        }

        for binary in ["image/png", "video/mp4", "application/octet-stream"] {
            assert!(
                !MimeType::parse(binary).expect("valid").is_text(),
                "{binary} should not be text-like"
            );
        }
    }

    #[test]
    fn well_known_constants_round_trip() {
        assert!(MimeType::octet_stream().is_unknown());
        assert!(!MimeType::octet_stream().is_directory());
        assert!(MimeType::directory().is_directory());
        assert_eq!(MimeType::directory().top_level(), "inode");
    }

    #[test]
    fn displays_as_the_full_type() {
        let m = MimeType::parse("image/png").expect("valid");
        assert_eq!(m.to_string(), "image/png");
    }
}
