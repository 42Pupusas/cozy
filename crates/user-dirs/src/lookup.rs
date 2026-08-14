//! [`DirLookup`] -- the `xdg-user-dir` lookup policy.
//!
//! # Why this is not [`UserDirs::path`]
//!
//! The two disagree on purpose, and the difference is load-bearing.
//!
//! [`UserDirs::path`] answers "where should Music live?" and falls back to
//! the spec's `~/Music`, because a sidebar wants a sensible place to offer.
//!
//! `xdg-user-dir MUSIC` answers "where did the user put Music?" and falls
//! back to `$HOME`, because a script that is about to write a file there
//! must not create `~/Music` as a side effect. `DESKTOP` is the one
//! exception, for historical compatibility.
//!
//! A caller that gets the wrong one of these scatters empty directories
//! through a home directory, so they stay separate methods.
//!
//! [`UserDirs::path`]: crate::UserDirs::path

use crate::config_paths::ConfigPaths;
use std::path::{Path, PathBuf};

/// Resolves one directory the way `xdg-user-dir` does.
pub struct DirLookup<'a> {
    paths: &'a ConfigPaths,
}

impl<'a> DirLookup<'a> {
    /// The one name that falls back to a subdirectory rather than `$HOME`.
    const HISTORICAL_EXCEPTION: &'static str = "DESKTOP";

    /// Look directories up against these locations.
    #[must_use]
    pub const fn new(paths: &'a ConfigPaths) -> Self {
        Self { paths }
    }

    /// Resolve `name`, which need not be one of the eight.
    ///
    /// An unrecognised name is not an error: the file may state any
    /// `XDG_*_DIR` key, and the tool prints `$HOME` when it does not.
    #[must_use]
    pub fn resolve(&self, name: &str) -> PathBuf {
        let contents = std::fs::read_to_string(self.paths.dirs_file()).unwrap_or_default();
        Self::find(&contents, name, self.paths.home()).unwrap_or_else(|| self.fallback(name))
    }

    /// `$HOME`, or `$HOME/Desktop` for the one historical exception.
    fn fallback(&self, name: &str) -> PathBuf {
        if name == Self::HISTORICAL_EXCEPTION {
            self.paths.home().join("Desktop")
        } else {
            self.paths.home().to_path_buf()
        }
    }

    /// Scan the file for `name`, following the reference parser's rules.
    ///
    /// A value must be quoted and either `$HOME`-relative or absolute;
    /// anything else is skipped rather than guessed at.
    #[must_use]
    pub fn find(contents: &str, name: &str, home: &Path) -> Option<PathBuf> {
        for line in contents.lines() {
            let line = line.trim_start();
            // `continue`, never `?`: the file opens with comment lines that
            // hold no `=`, and abandoning the scan at the first one made
            // every lookup fall back to `$HOME`.
            let Some((key, rest)) = line.split_once('=') else {
                continue;
            };
            let Some(key) = Self::key_type(key.trim_end()) else {
                continue;
            };
            if key != name {
                continue;
            }
            let Some(quoted) = rest.trim_start().strip_prefix('"') else {
                continue;
            };
            let value = Self::unescape_until_quote(quoted);
            if let Some(relative) = value.strip_prefix("$HOME/") {
                // `XDG_MUSIC_DIR="$HOME/"` is the documented way to say "no
                // separate directory", and it must print as the home path
                // with no trailing separator. Scripts interpolate this into
                // `$(xdg-user-dir MUSIC)/file`, so a stray slash shows up in
                // the paths they build.
                if relative.is_empty() {
                    return Some(home.to_path_buf());
                }
                return Some(home.join(relative));
            }
            if value.starts_with('/') {
                return Some(PathBuf::from(value));
            }
        }
        None
    }

    /// `XDG_MUSIC_DIR` becomes `MUSIC`; anything else is not a key.
    fn key_type(key: &str) -> Option<&str> {
        key.strip_prefix("XDG_")?.strip_suffix("_DIR")
    }

    /// Take the value up to the closing quote, undoing backslash escapes.
    fn unescape_until_quote(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        let mut chars = value.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => break,
                '\\' => match chars.next() {
                    Some(next) => out.push(next),
                    None => break,
                },
                _ => out.push(ch),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::DirLookup;
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn a_home_relative_value_resolves() {
        let found = DirLookup::find(r#"XDG_MUSIC_DIR="$HOME/Tunes""#, "MUSIC", &home());
        assert_eq!(found, Some(PathBuf::from("/home/tester/Tunes")));
    }

    #[test]
    fn an_absolute_value_resolves() {
        let found = DirLookup::find(r#"XDG_MUSIC_DIR="/mnt/music""#, "MUSIC", &home());
        assert_eq!(found, Some(PathBuf::from("/mnt/music")));
    }

    /// The reference parser requires a quoted, rooted value. A bare or
    /// relative one is skipped, not resolved against the current directory.
    #[test]
    fn an_unrooted_or_unquoted_value_is_skipped() {
        assert_eq!(
            DirLookup::find(r#"XDG_MUSIC_DIR="Tunes""#, "MUSIC", &home()),
            None
        );
        assert_eq!(
            DirLookup::find("XDG_MUSIC_DIR=$HOME/Tunes", "MUSIC", &home()),
            None
        );
    }

    #[test]
    fn an_escaped_quote_stays_in_the_value() {
        let found = DirLookup::find(r#"XDG_MUSIC_DIR="/mnt/My \"Music\"""#, "MUSIC", &home());
        assert_eq!(found, Some(PathBuf::from(r#"/mnt/My "Music""#)));
    }

    /// `$HOME/` alone means the home directory itself, and must not gain a
    /// trailing separator on the way out.
    #[test]
    fn a_bare_home_value_resolves_without_a_trailing_separator() {
        let found = DirLookup::find(r#"XDG_MUSIC_DIR="$HOME/""#, "MUSIC", &home());
        assert_eq!(found, Some(home()));
        assert_eq!(
            found.expect("a path").as_os_str().to_string_lossy(),
            "/home/tester",
            "a trailing slash would reach every script that interpolates this"
        );
    }

    /// The written file opens with comment lines that hold no `=`. Scanning
    /// must survive them rather than stopping at the first.
    #[test]
    fn a_leading_comment_block_does_not_end_the_scan() {
        let doc = "# a comment\n#\n\nXDG_MUSIC_DIR=\"$HOME/Tunes\"\n";
        assert_eq!(
            DirLookup::find(doc, "MUSIC", &home()),
            Some(PathBuf::from("/home/tester/Tunes"))
        );
    }

    /// A key for another directory must not satisfy this lookup.
    #[test]
    fn another_key_does_not_match() {
        let doc = "XDG_VIDEOS_DIR=\"$HOME/Films\"\nXDG_MUSIC_DIR=\"$HOME/Tunes\"\n";
        assert_eq!(
            DirLookup::find(doc, "MUSIC", &home()),
            Some(PathBuf::from("/home/tester/Tunes"))
        );
    }

    #[test]
    fn a_malformed_key_is_not_a_key() {
        assert_eq!(DirLookup::key_type("XDG_MUSIC_DIR"), Some("MUSIC"));
        assert_eq!(DirLookup::key_type("MUSIC"), None);
        assert_eq!(DirLookup::key_type("XDG_MUSIC"), None);
    }

    /// The fallback rule is the whole reason this type exists: everything
    /// falls back to `$HOME`, and `DESKTOP` alone does not.
    #[test]
    fn the_fallback_is_home_except_for_desktop() {
        let paths = crate::ConfigPaths::new(home(), "/nonexistent-config");
        let lookup = DirLookup::new(&paths);
        assert_eq!(lookup.resolve("MUSIC"), home());
        assert_eq!(lookup.resolve("DOCUMENTS"), home());
        assert_eq!(lookup.resolve("DESKTOP"), Path::new("/home/tester/Desktop"));
    }

    /// An unknown name is not an error; the tool prints `$HOME`.
    #[test]
    fn an_unknown_name_falls_back_to_home() {
        let paths = crate::ConfigPaths::new(home(), "/nonexistent-config");
        assert_eq!(DirLookup::new(&paths).resolve("NOSUCHTHING"), home());
    }
}
