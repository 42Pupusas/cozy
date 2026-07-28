//! The `.trashinfo` record: where a trashed file came from and when it was
//! deleted.
//!
//! Format, from the [freedesktop trash
//! spec](https://specifications.freedesktop.org/trash-spec/trashspec-latest.html)
//! and verified byte-for-byte against real files written by other tools on
//! this machine:
//!
//! ```text
//! [Trash Info]
//! Path=/home/user/Code/thing/src/native.rs
//! DeletionDate=2026-07-27T20:04:50
//! ```
//!
//! Two details are load-bearing:
//!
//! * `Path` is **percent-encoded** per RFC 2396. Without it, a filename
//!   containing a newline would inject a second key into the file, and a
//!   restore would send the file somewhere unintended.
//! * `DeletionDate` is **local** time with no `Z`/offset suffix (see
//!   [`localtime::Timestamp::trash_deletion_date`]).

use localtime::Timestamp;
use std::path::{Path, PathBuf};

/// One parsed `.trashinfo` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashInfo {
    /// The absolute path the file originally occupied, decoded.
    pub original_path: PathBuf,
    /// When it was trashed, local civil time.
    pub deleted_at: Timestamp,
}

impl TrashInfo {
    /// Build a record for `original_path` deleted at `deleted_at`.
    #[must_use]
    pub const fn new(original_path: PathBuf, deleted_at: Timestamp) -> Self {
        Self {
            original_path,
            deleted_at,
        }
    }

    /// Render the file's contents, ready to write.
    ///
    /// Ends with a trailing newline, matching what every other trash
    /// implementation emits.
    #[must_use]
    pub fn to_file_contents(&self) -> String {
        format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            Self::percent_encode(&self.original_path),
            self.deleted_at.trash_deletion_date()
        )
    }

    /// Parse a `.trashinfo` file's contents.
    ///
    /// `None` if the `[Trash Info]` group or the `Path` key is missing --
    /// without an original path the record is useless, since restoring is the
    /// only thing it exists to enable. A missing or malformed `DeletionDate`
    /// is tolerated (it degrades to the epoch) because it's metadata for
    /// display, not for correctness.
    #[must_use]
    pub fn parse(contents: &str) -> Option<Self> {
        let mut in_group = false;
        let mut path = None;
        let mut deleted_at = None;

        for line in contents.lines() {
            let line = line.trim();
            if line.eq_ignore_ascii_case("[Trash Info]") {
                in_group = true;
                continue;
            }
            // Any other group header ends ours. The spec defines only this
            // one group, but refusing to read keys outside it keeps a
            // hand-edited file from surprising us.
            if line.starts_with('[') {
                in_group = false;
                continue;
            }
            if !in_group {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                // First occurrence wins: a duplicated key is a malformed
                // file, and taking the first is the least surprising of the
                // available wrong answers.
                "Path" if path.is_none() => {
                    path = Some(Self::percent_decode(value.trim()));
                }
                "DeletionDate" if deleted_at.is_none() => {
                    deleted_at = Self::parse_deletion_date(value.trim());
                }
                _ => {}
            }
        }

        Some(Self {
            original_path: path?,
            deleted_at: deleted_at.unwrap_or(Timestamp {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
        })
    }

    /// Parse `YYYY-MM-DDThh:mm:ss` into a [`Timestamp`].
    ///
    /// Tolerates a trailing `Z` because some tools emit one even though the
    /// spec says local time -- we read what's there rather than rejecting the
    /// whole record over a suffix.
    fn parse_deletion_date(raw: &str) -> Option<Timestamp> {
        let raw = raw.strip_suffix('Z').unwrap_or(raw);
        let (date, time) = raw.split_once('T')?;

        let mut date_parts = date.split('-');
        let year = date_parts.next()?.parse().ok()?;
        let month = date_parts.next()?.parse().ok()?;
        let day = date_parts.next()?.parse().ok()?;

        let mut time_parts = time.split(':');
        let hour = time_parts.next()?.parse().ok()?;
        let minute = time_parts.next()?.parse().ok()?;
        let second = time_parts.next()?.parse().ok()?;

        Some(Timestamp {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }

    /// Percent-encode a path per RFC 2396, as the spec's `Path` key requires.
    ///
    /// Unreserved characters (alphanumerics and `-_.!~*'()`) plus `/` are
    /// passed through; everything else becomes `%XX`. Keeping `/` literal is
    /// what makes real `.trashinfo` files readable, and matches what other
    /// implementations produce.
    fn percent_encode(path: &Path) -> String {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_os_str().as_bytes();
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            if Self::is_unreserved(b) {
                out.push(b as char);
            } else {
                out.push('%');
                out.push(
                    char::from_digit(u32::from(b >> 4), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit(u32::from(b & 0x0f), 16)
                        .unwrap_or('0')
                        .to_ascii_uppercase(),
                );
            }
        }
        out
    }

    /// Whether `b` may appear literally in an encoded path.
    const fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' | b'/'
            )
    }

    /// Decode a percent-encoded path.
    ///
    /// Works on raw bytes rather than `char`s because a Unix path is bytes,
    /// not necessarily UTF-8 -- decoding `%FF` must produce that byte, not a
    /// replacement character. A stray `%` not followed by two hex digits is
    /// kept literally: some tools write unencoded paths, and mangling them
    /// would be worse than accepting them.
    fn percent_decode(raw: &str) -> PathBuf {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let bytes = raw.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                // Narrowed to `u8` here rather than after shifting: a hex
                // digit is always < 16, so `(hi << 4) | lo` provably fits in
                // a byte, and doing the arithmetic in `u8` lets the compiler
                // see that instead of needing a truncating cast.
                let hi = (bytes[i + 1] as char)
                    .to_digit(16)
                    .and_then(|d| u8::try_from(d).ok());
                let lo = (bytes[i + 2] as char)
                    .to_digit(16)
                    .and_then(|d| u8::try_from(d).ok());
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        PathBuf::from(OsString::from_vec(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_time() -> Timestamp {
        Timestamp {
            year: 2026,
            month: 7,
            day: 27,
            hour: 20,
            minute: 4,
            second: 50,
        }
    }

    /// Byte-for-byte against a real file found in this machine's trash.
    #[test]
    fn renders_exactly_what_other_tools_write() {
        let info = TrashInfo::new(
            PathBuf::from("/home/cuarentaydos/Code/forfun/agent/crates/mcp-git/src/native.rs"),
            sample_time(),
        );
        assert_eq!(
            info.to_file_contents(),
            "[Trash Info]\n\
             Path=/home/cuarentaydos/Code/forfun/agent/crates/mcp-git/src/native.rs\n\
             DeletionDate=2026-07-27T20:04:50\n"
        );
    }

    #[test]
    fn parses_a_real_file() {
        let info = TrashInfo::parse(
            "[Trash Info]\n\
             Path=/home/cuarentaydos/.cargo/bin/monitor\n\
             DeletionDate=2026-07-27T20:21:48\n",
        )
        .expect("valid record");
        assert_eq!(
            info.original_path,
            PathBuf::from("/home/cuarentaydos/.cargo/bin/monitor")
        );
        assert_eq!(info.deleted_at.hour, 20);
        assert_eq!(info.deleted_at.minute, 21);
        assert_eq!(info.deleted_at.second, 48);
    }

    #[test]
    fn round_trips_a_plain_path() {
        let info = TrashInfo::new(PathBuf::from("/home/user/notes.txt"), sample_time());
        let parsed = TrashInfo::parse(&info.to_file_contents()).expect("round trip");
        assert_eq!(parsed, info);
    }

    #[test]
    fn slashes_stay_literal_so_files_remain_readable() {
        let info = TrashInfo::new(PathBuf::from("/a/b/c"), sample_time());
        assert!(info.to_file_contents().contains("Path=/a/b/c\n"));
    }

    #[test]
    fn spaces_are_encoded() {
        let info = TrashInfo::new(PathBuf::from("/home/user/my file.txt"), sample_time());
        assert!(
            info.to_file_contents()
                .contains("Path=/home/user/my%20file.txt"),
            "got {}",
            info.to_file_contents()
        );
    }

    /// The security-relevant case: a newline in a filename must not be able
    /// to inject another key into the record.
    #[test]
    fn newlines_in_names_cannot_inject_extra_keys() {
        let evil = PathBuf::from("/tmp/a\nPath=/etc/passwd");
        let info = TrashInfo::new(evil.clone(), sample_time());
        let text = info.to_file_contents();

        assert!(!text.contains("\nPath=/etc/passwd"), "injection: {text}");
        assert_eq!(text.matches("Path=").count(), 1, "exactly one Path key");

        // And it must survive a round trip intact.
        let parsed = TrashInfo::parse(&text).expect("round trip");
        assert_eq!(parsed.original_path, evil);
    }

    #[test]
    fn percent_signs_in_names_round_trip() {
        let path = PathBuf::from("/tmp/100%25 done");
        let info = TrashInfo::new(path.clone(), sample_time());
        let parsed = TrashInfo::parse(&info.to_file_contents()).expect("round trip");
        assert_eq!(parsed.original_path, path, "a literal % must not be lost");
    }

    #[test]
    fn non_utf8_paths_round_trip() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // 0xFF is not valid UTF-8 but is a perfectly legal path byte.
        let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xFF]));
        let info = TrashInfo::new(path.clone(), sample_time());
        let text = info.to_file_contents();
        assert!(
            text.contains("%FF"),
            "should hex-escape the raw byte: {text}"
        );

        let parsed = TrashInfo::parse(&text).expect("round trip");
        assert_eq!(parsed.original_path, path);
    }

    #[test]
    fn unencoded_paths_from_other_tools_are_still_readable() {
        // Some implementations write the path verbatim; refusing to read that
        // would strand files in the trash forever.
        let info = TrashInfo::parse("[Trash Info]\nPath=/tmp/my file.txt\n").expect("lenient");
        assert_eq!(info.original_path, PathBuf::from("/tmp/my file.txt"));
    }

    #[test]
    fn a_trailing_z_on_the_date_is_tolerated() {
        let info =
            TrashInfo::parse("[Trash Info]\nPath=/tmp/x\nDeletionDate=2026-07-27T20:04:50Z\n")
                .expect("valid");
        assert_eq!(info.deleted_at.second, 50);
    }

    #[test]
    fn missing_path_is_rejected() {
        assert!(TrashInfo::parse("[Trash Info]\nDeletionDate=2026-07-27T20:04:50\n").is_none());
        assert!(TrashInfo::parse("").is_none());
    }

    #[test]
    fn missing_group_header_is_rejected() {
        // Keys outside the group must not be honoured.
        assert!(TrashInfo::parse("Path=/tmp/x\n").is_none());
    }

    #[test]
    fn keys_after_another_group_are_ignored() {
        assert!(TrashInfo::parse("[Other]\nPath=/tmp/x\n").is_none());
    }

    #[test]
    fn a_malformed_date_degrades_instead_of_failing() {
        let info =
            TrashInfo::parse("[Trash Info]\nPath=/tmp/x\nDeletionDate=not-a-date\n").expect("kept");
        assert_eq!(info.original_path, PathBuf::from("/tmp/x"));
        assert_eq!(info.deleted_at.year, 1970, "degrades to the epoch");
    }

    #[test]
    fn duplicate_keys_take_the_first() {
        let info =
            TrashInfo::parse("[Trash Info]\nPath=/tmp/first\nPath=/tmp/second\n").expect("valid");
        assert_eq!(info.original_path, PathBuf::from("/tmp/first"));
    }

    #[test]
    fn a_truncated_escape_is_kept_literally_rather_than_panicking() {
        // `%4` at end-of-string, and a `%zz` that isn't hex.
        let info = TrashInfo::parse("[Trash Info]\nPath=/tmp/a%4\n").expect("valid");
        assert_eq!(info.original_path, PathBuf::from("/tmp/a%4"));
        let info = TrashInfo::parse("[Trash Info]\nPath=/tmp/a%zz\n").expect("valid");
        assert_eq!(info.original_path, PathBuf::from("/tmp/a%zz"));
    }
}
