//! [`Target`] -- what the user actually named: a file, or a URL.
//!
//! # Why this is a type and not a `&str`
//!
//! `xdg-open` takes "a file or a URL", and the two resolve through
//! completely different keys: a file by its detected media type
//! (`image/png`), a URL by the pseudo-type `x-scheme-handler/<scheme>`.
//! Deciding which one a bare command-line argument is happens exactly once,
//! here, so `bins/open` and the portal's `OpenURI` cannot disagree about it.
//!
//! # The `file:` case
//!
//! A `file:` URL is a file, not a URL, and collapsing it here means nothing
//! downstream has to know the scheme exists. Callers hand us
//! `file:///tmp/my%20notes.txt` and get a path back.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// Something to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A filesystem path.
    File(PathBuf),
    /// A URL, with its scheme split out because that is the association key.
    Url {
        /// The URL as given, which is what gets passed to the handler.
        raw: String,
        /// Lowercased scheme, without the `:`.
        scheme: String,
    },
}

impl Target {
    /// Classify one command-line argument.
    ///
    /// Existence is checked first, and deliberately so. A scheme is only a
    /// syntactic guess, and a real file on disk is the stronger evidence:
    /// it makes a file legitimately named `draft:2` open as itself rather
    /// than as a `draft:` URL. Nothing costly hides in the check, because a
    /// genuine URL never names an existing path.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let path = Path::new(raw);
        if path.exists() {
            return Self::File(path.to_path_buf());
        }
        match Self::scheme_of(raw) {
            Some(scheme) if scheme == "file" => Self::File(Self::path_from_file_url(raw)),
            Some(scheme) => Self::Url {
                raw: raw.to_owned(),
                scheme,
            },
            None => Self::File(path.to_path_buf()),
        }
    }

    /// The scheme, for a URL.
    #[must_use]
    pub fn scheme(&self) -> Option<&str> {
        match self {
            Self::File(_) => None,
            Self::Url { scheme, .. } => Some(scheme),
        }
    }

    /// The path, for a file.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Url { .. } => None,
        }
    }

    /// What to hand the chosen application.
    #[must_use]
    pub fn as_argument(&self) -> &OsStr {
        match self {
            Self::File(path) => path.as_os_str(),
            Self::Url { raw, .. } => OsStr::new(raw),
        }
    }

    /// The scheme of `raw`, if it starts with a well-formed one.
    ///
    /// RFC 3986: a letter, then letters, digits, `+`, `-`, or `.`. The
    /// leading-letter rule is what stops an absolute path containing a colon
    /// (`/srv/backup:old/notes`) from parsing as a scheme.
    fn scheme_of(raw: &str) -> Option<String> {
        let colon = raw.find(':')?;
        let candidate = &raw[..colon];
        let mut chars = candidate.chars();
        if !chars.next()?.is_ascii_alphabetic() {
            return None;
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
            return None;
        }
        Some(candidate.to_lowercase())
    }

    /// Turn `file:///tmp/a`, `file://localhost/tmp/a`, or `file:/tmp/a` into
    /// `/tmp/a`.
    fn path_from_file_url(raw: &str) -> PathBuf {
        let rest = raw.split_once(':').map_or(raw, |(_, rest)| rest);
        // An authority (`//host`) is dropped: the only one we can serve is
        // this machine, and both `localhost` and the empty host mean that.
        let body = rest.strip_prefix("//").map_or(rest, |after| {
            after.find('/').map_or("", |slash| &after[slash..])
        });
        PathBuf::from(Self::decode_percent(body))
    }

    /// Percent-decode into an `OsString`.
    ///
    /// Byte-level rather than through `String`, because a path is bytes on
    /// this platform and a percent-escape can legitimately encode a sequence
    /// that is not valid UTF-8. Decoding through a lossy conversion would
    /// silently rename the file being opened.
    fn decode_percent(raw: &str) -> OsString {
        let bytes = raw.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if let Some(byte) = Self::escape_at(bytes, i) {
                out.push(byte);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        OsString::from_vec(out)
    }

    /// The byte a `%XX` escape at `i` denotes, if there is one.
    fn escape_at(bytes: &[u8], i: usize) -> Option<u8> {
        if bytes[i] != b'%' || i + 2 >= bytes.len() {
            return None;
        }
        let high = Self::hex_value(bytes[i + 1])?;
        let low = Self::hex_value(bytes[i + 2])?;
        Some(high * 16 + low)
    }

    const fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_keeps_its_scheme_and_its_raw_form() {
        let target = Target::parse("https://example.com/a?b=1");
        assert_eq!(target.scheme(), Some("https"));
        assert_eq!(target.as_argument(), "https://example.com/a?b=1");
    }

    #[test]
    fn a_scheme_is_matched_case_insensitively() {
        assert_eq!(Target::parse("HTTPS://example.com").scheme(), Some("https"));
    }

    /// `mailto:` has no `//`. A parser that keyed on `://` would miss the
    /// single most common non-http handler on a desktop.
    #[test]
    fn an_authority_less_url_is_still_a_url() {
        let target = Target::parse("mailto:someone@example.com");
        assert_eq!(target.scheme(), Some("mailto"));
        assert_eq!(target.as_argument(), "mailto:someone@example.com");
    }

    #[test]
    fn a_relative_path_is_a_file() {
        let target = Target::parse("notes/todo.txt");
        assert_eq!(target.path(), Some(Path::new("notes/todo.txt")));
        assert_eq!(target.scheme(), None);
    }

    /// The rule that keeps a colon in a pathname from inventing a scheme.
    #[test]
    fn an_absolute_path_containing_a_colon_is_not_a_url() {
        let target = Target::parse("/srv/backup:old/notes.txt");
        assert_eq!(target.path(), Some(Path::new("/srv/backup:old/notes.txt")));
    }

    /// A file that exists outranks a syntactic scheme match, which is the
    /// whole reason existence is tested first.
    #[test]
    fn an_existing_file_outranks_a_scheme_shaped_name() {
        let dir = std::env::temp_dir().join(format!("opener-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("draft:2");
        std::fs::write(&file, b"x").expect("write");

        let raw = file.to_string_lossy().into_owned();
        assert_eq!(Target::parse(&raw).path(), Some(file.as_path()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_url_becomes_a_plain_path() {
        assert_eq!(
            Target::parse("file:///tmp/notes.txt").path(),
            Some(Path::new("/tmp/notes.txt"))
        );
    }

    #[test]
    fn a_file_url_with_a_localhost_authority_becomes_a_plain_path() {
        assert_eq!(
            Target::parse("file://localhost/tmp/notes.txt").path(),
            Some(Path::new("/tmp/notes.txt"))
        );
    }

    #[test]
    fn a_file_url_without_an_authority_becomes_a_plain_path() {
        assert_eq!(
            Target::parse("file:/tmp/notes.txt").path(),
            Some(Path::new("/tmp/notes.txt"))
        );
    }

    #[test]
    fn a_file_url_is_percent_decoded() {
        assert_eq!(
            Target::parse("file:///tmp/my%20holiday%20photo.png").path(),
            Some(Path::new("/tmp/my holiday photo.png"))
        );
    }

    /// A literal `%` that is not an escape must survive untouched, or a file
    /// named `100%.txt` stops opening.
    #[test]
    fn a_lone_percent_is_left_alone() {
        assert_eq!(
            Target::parse("file:///tmp/100%.txt").path(),
            Some(Path::new("/tmp/100%.txt"))
        );
    }

    #[test]
    fn a_truncated_escape_is_left_alone() {
        assert_eq!(
            Target::parse("file:///tmp/a%2").path(),
            Some(Path::new("/tmp/a%2"))
        );
    }

    /// A percent-escape may encode bytes that are not valid UTF-8. Decoding
    /// through a lossy string conversion would replace them and open the
    /// wrong file -- or no file at all.
    #[test]
    fn a_non_utf8_escape_decodes_to_the_exact_bytes() {
        use std::os::unix::ffi::OsStrExt as _;
        let target = Target::parse("file:///tmp/caf%E9");
        let path = target.path().expect("a file url is a file");
        assert_eq!(path.as_os_str().as_bytes(), b"/tmp/caf\xe9");
    }
}
