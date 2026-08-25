//! Turning a mount point into a systemd unit name.
//!
//! A transient mount is created by name, and the name *is* the mount point:
//! `/run/media/bob/KINGSTON` must become `run-media-bob-KINGSTON.mount`. Get
//! the escaping wrong and systemd either rejects the name or mounts a
//! different path than the one asked for, so this mirrors systemd's own
//! `unit_name_path_escape` exactly rather than approximating it.
//!
//! The rules that are easy to get wrong, and are each pinned by a test:
//!
//! - `/` becomes `-`, so `-` itself must be escaped, or two different paths
//!   would collide on one unit name.
//! - A leading `.` is escaped, because a unit may not begin with a dot.
//! - Only `:`, `_`, `.` and alphanumerics survive; everything else, including
//!   every non-ASCII byte, becomes `\xNN`.

/// A systemd unit name derived from a filesystem path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitName {
    escaped: String,
}

impl UnitName {
    /// The characters systemd keeps verbatim, beyond alphanumerics.
    const KEPT: [char; 3] = [':', '_', '.'];

    /// Build the `.mount` unit name for `path`.
    ///
    /// Returns `None` for a relative path, since a mount unit name is only
    /// meaningful for an absolute one.
    #[must_use]
    pub fn for_mount_point(path: &str) -> Option<Self> {
        if !path.starts_with('/') {
            return None;
        }
        let mut escaped = Self::escape_path(path);
        escaped.push_str(".mount");
        Some(Self { escaped })
    }

    /// The escaped name, ready to pass to systemd.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.escaped
    }

    /// Consume into the owned name.
    #[must_use]
    pub fn into_string(self) -> String {
        self.escaped
    }

    /// Collapse repeated slashes and drop any trailing one, systemd's
    /// `path_simplify` step. The root stays `/`.
    fn simplify(path: &str) -> String {
        let joined = path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        if joined.is_empty() {
            String::from("/")
        } else {
            joined
        }
    }

    /// Escape a simplified path into a unit name body.
    fn escape_path(path: &str) -> String {
        let simplified = Self::simplify(path);
        if simplified == "/" {
            return String::from("-");
        }
        let mut out = String::new();
        for (index, ch) in simplified.chars().enumerate() {
            if index == 0 && ch == '.' {
                Self::push_hex(&mut out, ch);
            } else {
                Self::push_char(&mut out, ch);
            }
        }
        out
    }

    /// One character of the body.
    fn push_char(out: &mut String, ch: char) {
        if ch == '/' {
            out.push('-');
        } else if ch.is_ascii_alphanumeric() || Self::KEPT.contains(&ch) {
            out.push(ch);
        } else {
            Self::push_hex(out, ch);
        }
    }

    /// Escape one character as `\xNN` per UTF-8 byte, which is how systemd
    /// renders anything outside its allowed set.
    fn push_hex(out: &mut String, ch: char) {
        use std::fmt::Write;
        let mut buf = [0u8; 4];
        for byte in ch.encode_utf8(&mut buf).as_bytes() {
            let _ = write!(out, "\\x{byte:02x}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The everyday case: an automount path under `/run/media`.
    #[test]
    fn a_mount_point_becomes_a_dash_separated_unit() {
        let name = UnitName::for_mount_point("/run/media/bob/KINGSTON").expect("absolute");
        assert_eq!(name.as_str(), "run-media-bob-KINGSTON.mount");
    }

    /// The separator collision that makes escaping `-` mandatory: without
    /// it, these two distinct paths would produce one unit name.
    #[test]
    fn a_dash_in_a_label_cannot_collide_with_a_path_separator() {
        let dashed = UnitName::for_mount_point("/mnt/my-disk").expect("absolute");
        let nested = UnitName::for_mount_point("/mnt/my/disk").expect("absolute");
        assert_eq!(dashed.as_str(), "mnt-my\\x2ddisk.mount");
        assert_eq!(nested.as_str(), "mnt-my-disk.mount");
        assert_ne!(dashed, nested, "distinct paths must not share a unit name");
    }

    /// A unit name may not begin with a dot.
    #[test]
    fn a_leading_dot_is_escaped() {
        let name = UnitName::for_mount_point("/.hidden/disk").expect("absolute");
        assert!(name.as_str().starts_with("\\x2e"));
        assert_eq!(name.as_str(), "\\x2ehidden-disk.mount");
    }

    /// A dot elsewhere is ordinary and must survive untouched.
    #[test]
    fn an_interior_dot_is_kept() {
        let name = UnitName::for_mount_point("/mnt/disk.backup").expect("absolute");
        assert_eq!(name.as_str(), "mnt-disk.backup.mount");
    }

    /// A space is the most common label character needing an escape.
    #[test]
    fn a_space_is_escaped_as_hex() {
        let name = UnitName::for_mount_point("/media/My Disk").expect("absolute");
        assert_eq!(name.as_str(), "media-My\\x20Disk.mount");
    }

    /// A backslash must not pass through, or the escaping is ambiguous.
    #[test]
    fn a_backslash_is_itself_escaped() {
        let name = UnitName::for_mount_point("/mnt/a\\b").expect("absolute");
        assert_eq!(name.as_str(), "mnt-a\\x5cb.mount");
    }

    /// Non-ASCII labels are common on cameras and phones.
    #[test]
    fn a_non_ascii_label_escapes_every_byte() {
        let name = UnitName::for_mount_point("/media/Ünïcode").expect("absolute");
        assert_eq!(name.as_str(), "media-\\xc3\\x9cn\\xc3\\xafcode.mount");
    }

    /// The characters systemd keeps verbatim stay verbatim.
    #[test]
    fn the_permitted_punctuation_survives() {
        let name = UnitName::for_mount_point("/mnt/a:b_c").expect("absolute");
        assert_eq!(name.as_str(), "mnt-a:b_c.mount");
    }

    /// Redundant separators must not change the resulting unit.
    #[test]
    fn repeated_and_trailing_slashes_are_collapsed() {
        let plain = UnitName::for_mount_point("/run/media/bob").expect("absolute");
        for messy in ["/run//media/bob", "/run/media/bob/", "//run/media/bob//"] {
            let name = UnitName::for_mount_point(messy).expect("absolute");
            assert_eq!(name, plain, "{messy} must name the same unit");
        }
    }

    /// The root is the one path that escapes to a bare dash.
    #[test]
    fn the_root_is_a_single_dash() {
        let name = UnitName::for_mount_point("/").expect("absolute");
        assert_eq!(name.as_str(), "-.mount");
    }

    /// A relative path is not a mount point and must be refused rather than
    /// silently escaped into a plausible-looking unit.
    #[test]
    fn a_relative_path_is_refused() {
        assert!(UnitName::for_mount_point("run/media/bob").is_none());
        assert!(UnitName::for_mount_point("").is_none());
    }

    /// Every escape is lowercase hex of exactly two digits, so the output is
    /// parseable back by systemd's own unescape.
    #[test]
    fn every_escape_is_two_lowercase_hex_digits() {
        let name = UnitName::for_mount_point("/mnt/a b-c\\d").expect("absolute");
        let body = name.as_str();
        let mut rest = body;
        while let Some(at) = rest.find("\\x") {
            let digits = &rest[at + 2..at + 4];
            assert!(
                digits
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
                "{digits} must be lowercase hex"
            );
            rest = &rest[at + 4..];
        }
    }

    /// A name is always usable: non-empty and correctly suffixed.
    #[test]
    fn every_escaped_name_is_a_mount_unit() {
        for path in [
            "/",
            "/mnt",
            "/run/media/bob/KINGSTON",
            "/media/My Disk",
            "/.dotdir",
        ] {
            let name = UnitName::for_mount_point(path).expect("absolute");
            let suffixed = std::path::Path::new(name.as_str())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mount"));
            assert!(suffixed, "{path}");
            assert!(!name.as_str().starts_with('.'), "{path}");
            assert!(name.as_str().len() > ".mount".len(), "{path}");
        }
    }
}
