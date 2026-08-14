//! [`ConfigPaths`] -- where the user-dirs files live.
//!
//! The reader and the writer must agree on this, and the tools must be
//! testable without touching a real home directory. Both needs are met by
//! making the locations a value that tests can construct directly.

use std::path::{Path, PathBuf};

/// The home and config directories the tools work against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    home: PathBuf,
    config_home: PathBuf,
}

impl ConfigPaths {
    /// The file that records the resolved directories.
    pub const DIRS_FILE: &'static str = "user-dirs.dirs";
    /// The file that records the locale the names were translated for.
    pub const LOCALE_FILE: &'static str = "user-dirs.locale";
    /// The file that enables or disables the update tool.
    pub const CONF_FILE: &'static str = "user-dirs.conf";

    /// Read the environment, applying the XDG base-directory defaults.
    ///
    /// The spec requires `XDG_CONFIG_HOME` to be an absolute path and says a
    /// relative value must be ignored, so a relative one falls back to
    /// `$HOME/.config` rather than resolving against the current directory.
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Self { home, config_home }
    }

    /// Build explicit locations, for tests and for `--dummy-output`.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>, config_home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            config_home: config_home.into(),
        }
    }

    /// The home directory.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The configuration directory.
    #[must_use]
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    /// The path of `user-dirs.dirs`.
    #[must_use]
    pub fn dirs_file(&self) -> PathBuf {
        self.config_home.join(Self::DIRS_FILE)
    }

    /// The path of `user-dirs.locale`.
    #[must_use]
    pub fn locale_file(&self) -> PathBuf {
        self.config_home.join(Self::LOCALE_FILE)
    }

    /// The user's `user-dirs.conf`, which overrides the system-wide one.
    #[must_use]
    pub fn conf_file(&self) -> PathBuf {
        self.config_home.join(Self::CONF_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigPaths;
    use std::path::Path;

    #[test]
    fn the_files_sit_in_the_config_directory() {
        let paths = ConfigPaths::new("/home/tester", "/home/tester/.config");
        assert_eq!(
            paths.dirs_file(),
            Path::new("/home/tester/.config/user-dirs.dirs")
        );
        assert_eq!(
            paths.locale_file(),
            Path::new("/home/tester/.config/user-dirs.locale")
        );
        assert_eq!(
            paths.conf_file(),
            Path::new("/home/tester/.config/user-dirs.conf")
        );
    }

    /// A config home elsewhere moves every file with it; the home directory
    /// is a separate question from where the configuration lives.
    #[test]
    fn a_relocated_config_home_moves_the_files_but_not_home() {
        let paths = ConfigPaths::new("/home/tester", "/tmp/cfg");
        assert_eq!(paths.dirs_file(), Path::new("/tmp/cfg/user-dirs.dirs"));
        assert_eq!(paths.home(), Path::new("/home/tester"));
    }
}
