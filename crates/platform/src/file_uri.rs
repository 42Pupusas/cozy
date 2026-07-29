//! `file://` URIs and the `text/uri-list` MIME format.
//!
//! Drag-and-drop (and the clipboard, for "copy path") exchange file paths
//! with other applications as text, never as raw bytes -- `wl_data_source`
//! only ever offers MIME-typed strings. The universally understood MIME type
//! for "here are some files" is `text/uri-list`
//! ([RFC 2483](https://www.rfc-editor.org/rfc/rfc2483)): one URI per line,
//! lines separated by `\r\n`, blank lines and `#`-prefixed lines ignored.
//! Every file manager, browser, and chat client that accepts a dropped file
//! reads this format, so this module -- not some workspace-specific
//! encoding -- is what makes a drop onto Firefox or Mattermost legible to the
//! other side.
//!
//! Deliberately free of Wayland and I/O: encoding and decoding text is pure
//! logic, testable without a compositor or a filesystem.

use std::path::{Path, PathBuf};

/// A single `file://` URI: percent-encoding a filesystem path and back.
pub struct FileUri;

impl FileUri {
    /// Percent-encode `path` as a `file://` URI.
    ///
    /// Operates on raw bytes (`OsStr::as_encoded_bytes`), not `str`, so a
    /// path that isn't valid UTF-8 still round-trips -- Unix paths are only
    /// ever a bag of bytes, and refusing to drag a file for that reason
    /// would be a needless failure most users never hit but some would find
    /// utterly baffling.
    #[must_use]
    pub fn encode(path: &Path) -> String {
        use std::fmt::Write as _;
        let mut out = String::from("file://");
        for &b in path.as_os_str().as_encoded_bytes() {
            if Self::is_unreserved(b) {
                out.push(b as char);
            } else {
                let _ = write!(out, "%{b:02X}");
            }
        }
        out
    }

    /// Decode a `file://` URI back to a path. `None` for any other scheme,
    /// or for a percent-encoding that doesn't decode to valid path bytes.
    ///
    /// A `file://host/path` with a non-empty, non-`localhost` host is
    /// rejected rather than silently stripped: that host names a *different*
    /// machine, and treating its path as local would read (or worse, act on)
    /// the wrong file.
    #[must_use]
    pub fn decode(uri: &str) -> Option<PathBuf> {
        let rest = uri.strip_prefix("file://")?;
        // Distinguish "file:///path" (empty host) from "file://host/path":
        // the path always starts at the first '/' after the authority.
        let slash = rest.find('/')?;
        let host = &rest[..slash];
        if !host.is_empty() && host != "localhost" {
            return None;
        }
        let path_part = &rest[slash..];
        let bytes = Self::percent_decode(path_part)?;
        Some(PathBuf::from(std::ffi::OsString::from_vec_bytes(bytes)))
    }

    /// RFC 3986 unreserved characters, plus `/` so path separators are not
    /// mangled into `%2F`.
    const fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
    }

    /// Decode `%XX` escapes in a byte string, leaving other bytes untouched.
    /// `None` on a truncated or non-hex escape.
    fn percent_decode(s: &str) -> Option<Vec<u8>> {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                let hex = bytes.get(i + 1..i + 3)?;
                let hex_str = std::str::from_utf8(hex).ok()?;
                out.push(u8::from_str_radix(hex_str, 16).ok()?);
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        Some(out)
    }
}

/// An extension trait standing in for the (unstable) `OsString` byte
/// constructor, so [`FileUri::decode`] stays on stable Rust.
///
/// Unix paths are just bytes with no encoding requirement at all, so this is
/// a lossless, infallible conversion on the only platform this workspace
/// targets -- there is no portable equivalent to fall back to.
trait OsStringFromBytes {
    fn from_vec_bytes(bytes: Vec<u8>) -> Self;
}

impl OsStringFromBytes for std::ffi::OsString {
    #[cfg(unix)]
    fn from_vec_bytes(bytes: Vec<u8>) -> Self {
        use std::os::unix::ffi::OsStringExt;
        Self::from_vec(bytes)
    }
}

/// The `text/uri-list` MIME body: a newline-separated list of URIs.
pub struct UriList;

impl UriList {
    /// The MIME type this format is registered under.
    pub const MIME_TYPE: &'static str = "text/uri-list";

    /// Encode `paths` as a `text/uri-list` body, `\r\n`-separated per the
    /// spec (bare `\n` is a common but nonconformant shortcut some readers
    /// reject).
    #[must_use]
    pub fn encode(paths: &[PathBuf]) -> String {
        let mut out = String::new();
        for path in paths {
            out.push_str(&FileUri::encode(path));
            out.push_str("\r\n");
        }
        out
    }

    /// Decode a `text/uri-list` body into paths.
    ///
    /// Blank lines and `#`-prefixed comment lines are skipped per the spec.
    /// A line that isn't a `file://` URI (some other scheme, or garbage) is
    /// skipped rather than aborting the whole list -- a browser's drop can
    /// legitimately mix a `file://` entry with an `http://` one, and one
    /// unusable line must not lose every other file being dropped.
    #[must_use]
    pub fn decode(body: &str) -> Vec<PathBuf> {
        body.split(['\r', '\n'])
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(FileUri::decode)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_path_encodes_with_no_escapes() {
        assert_eq!(
            FileUri::encode(Path::new("/home/user/report.pdf")),
            "file:///home/user/report.pdf"
        );
    }

    #[test]
    fn spaces_and_special_characters_are_percent_encoded() {
        assert_eq!(
            FileUri::encode(Path::new("/tmp/My File (1).txt")),
            "file:///tmp/My%20File%20%281%29.txt"
        );
    }

    #[test]
    fn encoding_and_decoding_round_trips() {
        let path = Path::new("/home/user/Documents/résumé (final).docx");
        let uri = FileUri::encode(path);
        assert_eq!(FileUri::decode(&uri).as_deref(), Some(path));
    }

    #[test]
    fn decode_rejects_a_non_file_scheme() {
        assert_eq!(FileUri::decode("https://example.com/a.txt"), None);
    }

    #[test]
    fn decode_accepts_an_explicit_localhost() {
        assert_eq!(
            FileUri::decode("file://localhost/etc/hosts").as_deref(),
            Some(Path::new("/etc/hosts"))
        );
    }

    #[test]
    fn decode_rejects_a_remote_host() {
        // A path on another machine must not be silently read as local.
        assert_eq!(FileUri::decode("file://otherhost/etc/hosts"), None);
    }

    #[test]
    fn decode_rejects_a_truncated_percent_escape() {
        assert_eq!(FileUri::decode("file:///tmp/bad%2"), None);
    }

    #[test]
    fn decode_rejects_a_non_hex_percent_escape() {
        assert_eq!(FileUri::decode("file:///tmp/bad%zz"), None);
    }

    #[test]
    fn uri_list_encodes_multiple_paths_crlf_separated() {
        let paths = vec![PathBuf::from("/a.txt"), PathBuf::from("/b.txt")];
        assert_eq!(
            UriList::encode(&paths),
            "file:///a.txt\r\nfile:///b.txt\r\n"
        );
    }

    #[test]
    fn uri_list_decodes_multiple_lines() {
        let body = "file:///a.txt\r\nfile:///b.txt\r\n";
        assert_eq!(
            UriList::decode(body),
            vec![PathBuf::from("/a.txt"), PathBuf::from("/b.txt")]
        );
    }

    #[test]
    fn uri_list_skips_comments_and_blank_lines() {
        let body = "# a comment\r\n\r\nfile:///a.txt\r\n\r\n# trailing\r\n";
        assert_eq!(UriList::decode(body), vec![PathBuf::from("/a.txt")]);
    }

    #[test]
    fn uri_list_skips_an_unusable_line_without_losing_the_rest() {
        // A browser's drop can mix schemes; one bad line must not void the list.
        let body = "http://example.com/x\r\nfile:///a.txt\r\n";
        assert_eq!(UriList::decode(body), vec![PathBuf::from("/a.txt")]);
    }

    #[test]
    fn uri_list_accepts_bare_newlines_too() {
        // Not spec-conformant to *write*, but real-world senders exist, so
        // reading must be forgiving of the shortcut.
        let body = "file:///a.txt\nfile:///b.txt\n";
        assert_eq!(
            UriList::decode(body),
            vec![PathBuf::from("/a.txt"), PathBuf::from("/b.txt")]
        );
    }

    #[test]
    fn an_empty_list_encodes_to_an_empty_string() {
        assert_eq!(UriList::encode(&[]), "");
        assert!(UriList::decode("").is_empty());
    }
}
