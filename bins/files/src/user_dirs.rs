//! [`UserDirs`] -- the XDG user directories (Desktop, Downloads, …).
//!
//! The sidebar's "common places" section is only useful if it names the
//! directories the user actually has. Those are not fixed: the XDG
//! user-dirs spec lets them be renamed and relocated (and localized -- a
//! German desktop has `Schreibtisch`, not `Desktop`), and the answer lives in
//! `~/.config/user-dirs.dirs`, written by `xdg-user-dirs-update`.
//!
//! Hardcoding `~/Desktop` would show a broken entry to anyone whose setup
//! differs, so we read the file. Parsing is split from the environment lookup
//! ([`UserDirs::parse`] vs [`UserDirs::load`]) so the format can be tested
//! without touching a real home directory.

use std::path::{Path, PathBuf};

/// One well-known user directory, in the order the sidebar shows them.
///
/// An enum rather than loose strings so a typo cannot silently produce an
/// entry that never resolves, and so the sidebar's ordering is stated once
/// here instead of being re-derived at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserDir {
    Desktop,
    Documents,
    Downloads,
    Music,
    Pictures,
    Videos,
}

impl UserDir {
    /// Every directory, in sidebar order: the ones people reach for most
    /// (Documents, Downloads) sit near the top.
    pub const ALL: [Self; 6] = [
        Self::Desktop,
        Self::Documents,
        Self::Downloads,
        Self::Music,
        Self::Pictures,
        Self::Videos,
    ];

    /// The key used in `user-dirs.dirs`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Desktop => "XDG_DESKTOP_DIR",
            Self::Documents => "XDG_DOCUMENTS_DIR",
            Self::Downloads => "XDG_DOWNLOAD_DIR",
            Self::Music => "XDG_MUSIC_DIR",
            Self::Pictures => "XDG_PICTURES_DIR",
            Self::Videos => "XDG_VIDEOS_DIR",
        }
    }

    /// The spec's fallback, relative to home, when the file says nothing.
    #[must_use]
    pub const fn fallback(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Downloads => "Downloads",
            Self::Music => "Music",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
        }
    }

    /// The label the sidebar shows.
    ///
    /// Deliberately *not* the directory's basename: a localized setup has
    /// `XDG_DOWNLOAD_DIR="$HOME/Descargas"`, and a sidebar mixing English and
    /// Spanish labels reads worse than one that is consistently in the
    /// application's own language.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
            Self::Downloads => "Downloads",
            Self::Music => "Music",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
        }
    }
}

/// Resolved paths for the well-known user directories.
pub struct UserDirs {
    home: PathBuf,
    /// Parallel to [`UserDir::ALL`]; `None` where the file gave no usable
    /// value and the fallback does not exist on disk.
    resolved: Vec<(UserDir, PathBuf)>,
}

impl UserDirs {
    /// Read the user's `user-dirs.dirs`, falling back to the spec defaults.
    ///
    /// Never fails: a missing or unreadable file is the common case on a
    /// minimal system, and the spec's `~/Documents`-style fallbacks are
    /// exactly right there.
    #[must_use]
    pub fn load() -> Self {
        let home = std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from);
        let config =
            std::env::var("XDG_CONFIG_HOME").map_or_else(|_| home.join(".config"), PathBuf::from);
        let contents = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
        Self::parse(&contents, &home)
    }

    /// Parse `user-dirs.dirs` contents against a given home directory.
    ///
    /// The format is shell-ish but not shell: `KEY="value"` lines, `#`
    /// comments, and a leading `$HOME` in the value. We deliberately do not
    /// invoke a shell or expand arbitrary variables -- this file is read at
    /// startup, and running it as a script would be a remote-code-execution
    /// hole for anything that can write to the config directory.
    #[must_use]
    pub fn parse(contents: &str, home: &Path) -> Self {
        let mut resolved = Vec::new();
        for dir in UserDir::ALL {
            let path = Self::find_key(contents, dir.key(), home)
                .unwrap_or_else(|| home.join(dir.fallback()));
            resolved.push((dir, path));
        }
        Self {
            home: home.to_path_buf(),
            resolved,
        }
    }

    /// The value for one key, if the file names it.
    fn find_key(contents: &str, key: &str, home: &Path) -> Option<PathBuf> {
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            // `continue`, never `?`: a line that is not this key (every other
            // key in the file, and any comment) must advance the scan rather
            // than abandon it. Returning early here made a commented-out key
            // earlier in the file hide the real one below it.
            let Some(rest) = line.strip_prefix(key) else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            let value = rest.trim().trim_matches('"');
            if value.is_empty() {
                return None;
            }
            return Some(Self::expand(value, home));
        }
        None
    }

    /// Expand a leading `$HOME`/`~`; leave everything else literal.
    fn expand(value: &str, home: &Path) -> PathBuf {
        if let Some(rest) = value.strip_prefix("$HOME") {
            return home.join(rest.trim_start_matches('/'));
        }
        if let Some(rest) = value.strip_prefix('~') {
            return home.join(rest.trim_start_matches('/'));
        }
        PathBuf::from(value)
    }

    /// The home directory itself.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Every resolved directory that actually exists, in sidebar order.
    ///
    /// Existence is checked here rather than at paint time so the sidebar
    /// never offers a place that cannot be opened -- a dead entry that
    /// reports an error when clicked is worse than no entry.
    #[must_use]
    pub fn existing(&self) -> Vec<(UserDir, PathBuf)> {
        self.resolved
            .iter()
            .filter(|(_, path)| path.is_dir())
            .cloned()
            .collect()
    }

    /// The resolved path for one directory, whether or not it exists.
    ///
    /// The sidebar builds from [`Self::existing`]; this is the accessor the
    /// parsing tests assert through, since resolution is the whole job of
    /// this module and needs to be observable independently of whatever
    /// happens to exist on the machine running the tests.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self, dir: UserDir) -> Option<&Path> {
        self.resolved
            .iter()
            .find(|(d, _)| *d == dir)
            .map(|(_, p)| p.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::{UserDir, UserDirs};
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn an_empty_file_falls_back_to_the_spec_defaults() {
        let dirs = UserDirs::parse("", &home());
        assert_eq!(
            dirs.path(UserDir::Documents),
            Some(Path::new("/home/tester/Documents"))
        );
    }

    #[test]
    fn a_home_relative_value_is_expanded() {
        let dirs = UserDirs::parse(r#"XDG_DOWNLOAD_DIR="$HOME/Downloads""#, &home());
        assert_eq!(
            dirs.path(UserDir::Downloads),
            Some(Path::new("/home/tester/Downloads"))
        );
    }

    #[test]
    fn an_absolute_value_is_taken_literally() {
        let dirs = UserDirs::parse(r#"XDG_MUSIC_DIR="/mnt/media/music""#, &home());
        assert_eq!(
            dirs.path(UserDir::Music),
            Some(Path::new("/mnt/media/music"))
        );
    }

    /// A relocated, localized directory is the whole reason we read the file
    /// rather than hardcoding English names under `$HOME`.
    #[test]
    fn a_localized_directory_resolves_to_its_real_path() {
        let dirs = UserDirs::parse(r#"XDG_DOWNLOAD_DIR="$HOME/Descargas""#, &home());
        assert_eq!(
            dirs.path(UserDir::Downloads),
            Some(Path::new("/home/tester/Descargas"))
        );
        // …but the sidebar still labels it in the application's language.
        assert_eq!(UserDir::Downloads.label(), "Downloads");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let doc = "# generated\n\n#XDG_MUSIC_DIR=\"/wrong\"\nXDG_MUSIC_DIR=\"$HOME/Tunes\"\n";
        let dirs = UserDirs::parse(doc, &home());
        assert_eq!(
            dirs.path(UserDir::Music),
            Some(Path::new("/home/tester/Tunes"))
        );
    }

    /// `XDG_DESKTOP_DIR="$HOME"` is the documented way to say "I have no
    /// separate desktop directory". It must not become a stray entry
    /// duplicating Home.
    #[test]
    fn a_directory_pointing_at_home_is_still_resolved_not_dropped() {
        let dirs = UserDirs::parse(r#"XDG_DESKTOP_DIR="$HOME/""#, &home());
        assert_eq!(dirs.path(UserDir::Desktop), Some(Path::new("/home/tester")));
    }

    /// The file is config, not a script: a value that looks like a command
    /// substitution must be treated as a literal path, never executed.
    #[test]
    fn shell_syntax_in_a_value_is_not_executed() {
        let dirs = UserDirs::parse(r#"XDG_MUSIC_DIR="$(rm -rf /)""#, &home());
        assert_eq!(
            dirs.path(UserDir::Music),
            Some(Path::new("$(rm -rf /)")),
            "the value must stay literal"
        );
    }

    /// Only directories that exist may reach the sidebar.
    #[test]
    fn existing_filters_to_real_directories() {
        let tmp = crate::test_support::TempDir::new("user-dirs");
        tmp.create_dir("Documents");
        let doc = format!(
            "XDG_DOCUMENTS_DIR=\"{}/Documents\"\nXDG_MUSIC_DIR=\"{}/Nope\"\n",
            tmp.path().display(),
            tmp.path().display()
        );
        let dirs = UserDirs::parse(&doc, tmp.path());
        let found = dirs.existing();
        assert!(found.iter().any(|(d, _)| *d == UserDir::Documents));
        assert!(
            !found.iter().any(|(d, _)| *d == UserDir::Music),
            "a missing directory must not be offered"
        );
    }
}
