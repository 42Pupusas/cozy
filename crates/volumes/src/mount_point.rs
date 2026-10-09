//! [`MountPoint`] -- where a device gets attached, derived safely from its
//! own label.
//!
//! # The label is attacker-controlled
//!
//! A volume label is a string stored *on the volume*, so it is chosen by
//! whoever formatted the stick, not by the user who plugged it in. The
//! obvious mount point is `/run/media/<user>/<label>`, and building that by
//! concatenation is a path-traversal hole: a stick labelled `../../../etc`
//! would ask a **privileged** mounter to attach a filesystem over `/etc`.
//!
//! So the label never reaches a path as text. It is reduced to a single path
//! component by [`Self::safe_component`], which:
//!
//! - replaces every separator and control character with `_`,
//! - refuses `.` and `..` outright,
//! - truncates to the 255-byte limit a path component has, on a character
//!   boundary,
//! - falls back to the device name, and then to a fixed word, when nothing
//!   usable survives.
//!
//! The result is joined to the root, and the constructor then *verifies* the
//! join landed directly under it. That check is redundant if the sanitiser is
//! correct, which is exactly why it is there.
//!
//! # Bidirectional overrides
//!
//! Unicode direction controls are stripped as well. They are not a traversal
//! risk; they are a **display** risk, and this crate feeds a file browser. A
//! label containing U+202E renders a mount point that reads as a different
//! name than it is, which is the same trick used to disguise executable
//! attachments.

use std::path::{Path, PathBuf};

/// A validated directory to mount a device on.
///
/// Constructible only through the checked builders, so a value of this type
/// is proof that the path is a single component under a known root.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MountPoint {
    path: PathBuf,
}

impl MountPoint {
    /// Where automounters place removable media.
    pub const DEFAULT_ROOT: &'static str = "/run/media";

    /// The name used when neither the label nor the device yields anything.
    const FALLBACK: &'static str = "disk";

    /// The maximum length of a single path component, in bytes.
    const MAX_COMPONENT: usize = 255;

    /// Unicode direction controls, which let a label render as a name it is
    /// not.
    const BIDI_CONTROLS: [char; 9] = [
        '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}',
    ];

    /// The mount point for `user`, under the standard media root.
    #[must_use]
    pub fn for_user(user: &str, label: Option<&str>, device: &str) -> Option<Self> {
        let root = PathBuf::from(Self::DEFAULT_ROOT).join(Self::safe_component(user)?);
        Self::under(&root, label, device)
    }

    /// The mount point under an explicit root.
    ///
    /// The root must be absolute: a relative mount point is meaningless to
    /// the privileged mounter, which has its own working directory.
    #[must_use]
    pub fn under(root: &Path, label: Option<&str>, device: &str) -> Option<Self> {
        if !root.is_absolute() {
            return None;
        }
        let name = label
            .and_then(Self::safe_component)
            .or_else(|| Self::device_component(device))
            .unwrap_or_else(|| Self::FALLBACK.to_owned());
        let path = root.join(&name);
        (path.parent() == Some(root)).then_some(Self { path })
    }

    /// The bare device name, e.g. `sdb1` from `/dev/sdb1`.
    fn device_component(device: &str) -> Option<String> {
        Path::new(device)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(Self::safe_component)
    }

    /// Reduce arbitrary text to one safe path component.
    fn safe_component(text: &str) -> Option<String> {
        let mapped: String = text
            .chars()
            .filter(|c| !c.is_control() && !Self::BIDI_CONTROLS.contains(c))
            .map(|c| if std::path::is_separator(c) { '_' } else { c })
            .collect();
        let trimmed = Self::truncate(mapped.trim());
        if trimmed.is_empty() || trimmed.chars().all(|c| c == '.') {
            return None;
        }
        Some(trimmed.to_owned())
    }

    /// Cut to the component length limit without splitting a character.
    fn truncate(text: &str) -> &str {
        if text.len() <= Self::MAX_COMPONENT {
            return text;
        }
        let mut end = Self::MAX_COMPONENT;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    /// The directory to mount on.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path as text, which is what a mounter's `Where` field wants.
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.path.to_string_lossy()
    }

    /// The last component -- the name the user will see in the sidebar.
    #[must_use]
    pub fn name(&self) -> std::borrow::Cow<'_, str> {
        self.path
            .file_name()
            .map_or_else(|| std::borrow::Cow::Borrowed(""), |n| n.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use super::MountPoint;
    use std::path::Path;

    const ROOT: &str = "/run/media/bob";

    fn under(label: &str) -> MountPoint {
        MountPoint::under(Path::new(ROOT), Some(label), "/dev/sdb1").expect("an absolute root")
    }

    #[test]
    fn an_ordinary_label_becomes_its_own_directory() {
        assert_eq!(
            under("KINGSTON").path(),
            Path::new("/run/media/bob/KINGSTON")
        );
    }

    /// The hole this type exists to close. A privileged mounter told to
    /// attach a filesystem at `/etc` is a compromised machine.
    #[test]
    fn a_traversing_label_cannot_escape_the_root() {
        for label in [
            "../../../etc",
            "..",
            "../..",
            "a/../../b",
            "/etc/shadow",
            "....//....//etc",
        ] {
            let point = under(label);
            assert_eq!(
                point.path().parent(),
                Some(Path::new(ROOT)),
                "{label} escaped to {}",
                point.path().display()
            );
            assert!(
                !point
                    .path()
                    .components()
                    .any(|c| c.as_os_str() == ".." || c.as_os_str() == "."),
                "{label} left a traversal in {}",
                point.path().display()
            );
        }
    }

    /// Every produced path must be exactly one component below the root,
    /// whatever the label. The property, rather than a list of examples.
    #[test]
    fn every_label_yields_exactly_one_component_under_the_root() {
        for label in [
            "KINGSTON",
            "../etc",
            "a/b/c/d",
            "",
            " ",
            ".",
            "..",
            "...",
            "\0evil",
            "with space",
            "Ünïcødé",
            "🦀",
            "/",
            "//",
            "a\\b",
        ] {
            let point = under(label);
            let extra = point
                .path()
                .strip_prefix(ROOT)
                .expect("must be under the root");
            assert_eq!(
                extra.components().count(),
                1,
                "{label:?} produced {}",
                point.path().display()
            );
        }
    }

    /// A separator inside a label is data, not structure.
    #[test]
    fn separators_in_a_label_become_underscores() {
        assert_eq!(under("a/b").name(), "a_b");
    }

    /// A label that sanitises to nothing must still mount somewhere, and the
    /// device name is the more informative fallback.
    #[test]
    fn an_empty_label_falls_back_to_the_device_name() {
        let point = MountPoint::under(Path::new(ROOT), None, "/dev/sdb1").expect("a root");
        assert_eq!(point.name(), "sdb1");
        assert_eq!(under("").name(), "sdb1", "an unusable label is no label");
        assert_eq!(under("   ").name(), "sdb1");
        assert_eq!(under(".").name(), "sdb1");
    }

    /// When the device name is unusable too, something must still be chosen
    /// rather than mounting on the root directory itself.
    #[test]
    fn a_useless_label_and_device_still_yield_a_directory() {
        let point = MountPoint::under(Path::new(ROOT), Some(".."), "..").expect("a root");
        assert_eq!(point.name(), "disk");
    }

    /// Control characters in a path are hostile to every tool that will
    /// later print it, and NUL truncates the path in the kernel.
    #[test]
    fn control_characters_are_removed() {
        let point = under("KING\u{0}STON\n");
        assert_eq!(point.name(), "KINGSTON");
    }

    /// A label may not disguise itself as a different name in the sidebar.
    #[test]
    fn direction_overrides_are_removed() {
        let point = under("photo\u{202e}gnp.exe");
        assert!(
            !point.name().contains('\u{202e}'),
            "a direction override survived: {:?}",
            point.name()
        );
        assert_eq!(point.name(), "photognp.exe");
    }

    /// A component longer than 255 bytes cannot be created at all, so the
    /// mount would fail with a confusing error rather than being trimmed.
    #[test]
    fn an_overlong_label_is_truncated_to_the_component_limit() {
        let point = under(&"x".repeat(400));
        assert_eq!(point.name().len(), 255);
    }

    /// Truncation must not slice a multi-byte character in half, which would
    /// produce a name that is not valid UTF-8.
    #[test]
    fn truncation_respects_character_boundaries() {
        let point = under(&"é".repeat(400));
        assert!(point.name().len() <= 255);
        assert!(
            point.name().chars().all(|c| c == 'é'),
            "a character was split: {:?}",
            point.name()
        );
    }

    /// Unicode labels are common and perfectly safe; they must survive.
    #[test]
    fn ordinary_unicode_survives_intact() {
        assert_eq!(under("Fotos Ürlaub 🏖").name(), "Fotos Ürlaub 🏖");
    }

    /// A relative root would be resolved against the mounter's working
    /// directory, not the caller's.
    #[test]
    fn a_relative_root_is_refused() {
        assert!(MountPoint::under(Path::new("run/media"), Some("A"), "/dev/sdb1").is_none());
    }

    /// The username is part of the path too, so it gets the same treatment.
    #[test]
    fn the_user_directory_is_sanitised_as_well() {
        let point = MountPoint::for_user("../root", Some("A"), "/dev/sdb1").expect("a user");
        assert_eq!(point.path(), Path::new("/run/media/.._root/A"));
    }

    #[test]
    fn the_standard_root_is_used_for_a_named_user() {
        let point = MountPoint::for_user("bob", Some("KINGSTON"), "/dev/sdb1").expect("a user");
        assert_eq!(point.path(), Path::new("/run/media/bob/KINGSTON"));
    }
}
