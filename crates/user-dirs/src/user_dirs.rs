//! [`UserDirs`] -- reading `user-dirs.dirs`.

use crate::config_paths::ConfigPaths;
use crate::user_dir::UserDir;
use std::path::{Path, PathBuf};

/// Resolved paths for the well-known user directories.
#[derive(Debug, Clone)]
pub struct UserDirs {
    home: PathBuf,
    /// Parallel to [`UserDir::ALL`], always complete: a key the file omits
    /// resolves to the spec's fallback.
    resolved: Vec<(UserDir, PathBuf)>,
    /// The keys the file stated, which the update tool must distinguish from
    /// the ones it supplied itself.
    stated: Vec<UserDir>,
    /// `XDG_*_DIR` keys outside the eight, kept verbatim.
    ///
    /// The format does not reserve the namespace, and real configurations
    /// carry extra keys. The update tool rewrites this file at every login,
    /// so a key it did not understand must survive rather than be deleted.
    extra: Vec<(String, String)>,
}

impl UserDirs {
    /// Read the user's `user-dirs.dirs`, falling back to the spec defaults.
    ///
    /// Never fails: a missing or unreadable file is the common case on a
    /// minimal system, and the spec's `~/Documents`-style fallbacks are
    /// exactly right there.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&ConfigPaths::from_env())
    }

    /// Read `user-dirs.dirs` from explicit locations.
    #[must_use]
    pub fn load_from(paths: &ConfigPaths) -> Self {
        let contents = std::fs::read_to_string(paths.dirs_file()).unwrap_or_default();
        Self::parse(&contents, paths.home())
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
        let mut resolved = Vec::with_capacity(UserDir::ALL.len());
        let mut stated = Vec::new();
        for dir in UserDir::ALL {
            match Self::find_key(contents, dir.key(), home) {
                Some(path) => {
                    stated.push(dir);
                    resolved.push((dir, path));
                }
                None => resolved.push((dir, home.join(dir.fallback()))),
            }
        }
        Self {
            home: home.to_path_buf(),
            resolved,
            stated,
            extra: Self::find_extra_keys(contents),
        }
    }

    /// Collect the `XDG_*_DIR` keys that are not one of the eight.
    ///
    /// The raw value is kept exactly as written: this crate does not model
    /// the key, so re-quoting it could only lose information.
    fn find_extra_keys(contents: &str) -> Vec<(String, String)> {
        let mut extra = Vec::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim_end();
            if !key.starts_with("XDG_") || !key.ends_with("_DIR") {
                continue;
            }
            if UserDir::from_key(key).is_some() {
                continue;
            }
            extra.push((key.to_owned(), value.trim().to_owned()));
        }
        extra
    }

    /// The `XDG_*_DIR` keys this crate does not model, with raw values.
    #[must_use]
    pub fn extra_keys(&self) -> &[(String, String)] {
        &self.extra
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
            let value = Self::unquote(rest.trim());
            if value.is_empty() {
                return None;
            }
            return Some(Self::expand(&value, home));
        }
        None
    }

    /// Strip the surrounding quotes and undo the writer's escaping.
    ///
    /// The escaping follows shell double-quote rules, which is what the
    /// format imitates: a backslash is literal unless it precedes one of the
    /// four characters that would otherwise carry meaning.
    fn unquote(value: &str) -> String {
        let inner = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(value);
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some(next @ ('"' | '\\' | '$' | '`')) => out.push(next),
                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            }
        }
        out
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

    /// Every resolved directory that exists on disk, in spec order.
    ///
    /// Existence is checked here rather than at paint time so a sidebar
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
    /// Always `Some`: every directory resolves, to the file's value or to
    /// the spec's fallback. It returns a plain path for that reason.
    #[must_use]
    pub fn path(&self, dir: UserDir) -> &Path {
        self.resolved
            .iter()
            .find(|(d, _)| *d == dir)
            .map_or(self.home.as_path(), |(_, p)| p.as_path())
    }

    /// Whether the file stated this directory, rather than it defaulting.
    #[must_use]
    pub fn is_stated(&self, dir: UserDir) -> bool {
        self.stated.contains(&dir)
    }

    /// Replace one directory's path.
    pub fn set(&mut self, dir: UserDir, path: impl Into<PathBuf>) {
        let path = path.into();
        if let Some(slot) = self.resolved.iter_mut().find(|(d, _)| *d == dir) {
            slot.1 = path;
        }
        if !self.stated.contains(&dir) {
            self.stated.push(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UserDirs;
    use crate::user_dir::UserDir;
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn an_empty_file_falls_back_to_the_spec_defaults() {
        let dirs = UserDirs::parse("", &home());
        assert_eq!(
            dirs.path(UserDir::Documents),
            Path::new("/home/tester/Documents")
        );
        assert!(!dirs.is_stated(UserDir::Documents));
    }

    #[test]
    fn a_home_relative_value_is_expanded() {
        let dirs = UserDirs::parse(r#"XDG_DOWNLOAD_DIR="$HOME/Downloads""#, &home());
        assert_eq!(
            dirs.path(UserDir::Download),
            Path::new("/home/tester/Downloads")
        );
        assert!(dirs.is_stated(UserDir::Download));
    }

    #[test]
    fn an_absolute_value_is_taken_literally() {
        let dirs = UserDirs::parse(r#"XDG_MUSIC_DIR="/mnt/media/music""#, &home());
        assert_eq!(dirs.path(UserDir::Music), Path::new("/mnt/media/music"));
    }

    /// A relocated, localized directory is the whole reason we read the file
    /// rather than hardcoding English names under `$HOME`.
    #[test]
    fn a_localized_directory_resolves_to_its_real_path() {
        let dirs = UserDirs::parse(r#"XDG_DOWNLOAD_DIR="$HOME/Descargas""#, &home());
        assert_eq!(
            dirs.path(UserDir::Download),
            Path::new("/home/tester/Descargas")
        );
        assert_eq!(UserDir::Download.label(), "Downloads");
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let doc = "# generated\n\n#XDG_MUSIC_DIR=\"/wrong\"\nXDG_MUSIC_DIR=\"$HOME/Tunes\"\n";
        let dirs = UserDirs::parse(doc, &home());
        assert_eq!(dirs.path(UserDir::Music), Path::new("/home/tester/Tunes"));
    }

    /// `XDG_DESKTOP_DIR="$HOME"` is the documented way to say "I have no
    /// separate desktop directory".
    #[test]
    fn a_directory_pointing_at_home_is_still_resolved_not_dropped() {
        let dirs = UserDirs::parse(r#"XDG_DESKTOP_DIR="$HOME/""#, &home());
        assert_eq!(dirs.path(UserDir::Desktop), Path::new("/home/tester"));
    }

    /// The file is config, not a script: a value that looks like a command
    /// substitution must be treated as a literal path, never executed.
    #[test]
    fn shell_syntax_in_a_value_is_not_executed() {
        let dirs = UserDirs::parse(r#"XDG_MUSIC_DIR="$(rm -rf /)""#, &home());
        assert_eq!(
            dirs.path(UserDir::Music),
            Path::new("$(rm -rf /)"),
            "the value must stay literal"
        );
    }

    /// An empty value means "no such directory" and must fall back rather
    /// than resolving to the home directory itself.
    #[test]
    fn an_empty_value_falls_back_instead_of_resolving_to_home() {
        let dirs = UserDirs::parse(r#"XDG_MUSIC_DIR="""#, &home());
        assert_eq!(dirs.path(UserDir::Music), Path::new("/home/tester/Music"));
        assert!(!dirs.is_stated(UserDir::Music));
    }

    #[test]
    fn setting_a_directory_marks_it_stated() {
        let mut dirs = UserDirs::parse("", &home());
        assert!(!dirs.is_stated(UserDir::Videos));
        dirs.set(UserDir::Videos, "/srv/video");
        assert_eq!(dirs.path(UserDir::Videos), Path::new("/srv/video"));
        assert!(dirs.is_stated(UserDir::Videos));
    }

    /// Every one of the eight resolves even from an empty file, so no caller
    /// has to handle a missing directory.
    #[test]
    fn every_directory_resolves_from_an_empty_file() {
        let dirs = UserDirs::parse("", &home());
        for dir in UserDir::ALL {
            assert_eq!(dirs.path(dir), home().join(dir.fallback()));
        }
    }
}
