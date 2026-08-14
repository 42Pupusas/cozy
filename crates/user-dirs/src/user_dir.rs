//! [`UserDir`] -- the eight well-known XDG user directories.

/// One well-known user directory.
///
/// An enum rather than loose strings so a typo cannot silently produce an
/// entry that never resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UserDir {
    Desktop,
    Download,
    Templates,
    PublicShare,
    Documents,
    Music,
    Pictures,
    Videos,
}

impl UserDir {
    /// Every directory, in the order `xdg-user-dirs-update` writes them.
    ///
    /// This is the spec's order, not a display order. A user interface that
    /// shows a subset states its own sequence.
    pub const ALL: [Self; 8] = [
        Self::Desktop,
        Self::Download,
        Self::Templates,
        Self::PublicShare,
        Self::Documents,
        Self::Music,
        Self::Pictures,
        Self::Videos,
    ];

    /// The key used in `user-dirs.dirs`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Desktop => "XDG_DESKTOP_DIR",
            Self::Download => "XDG_DOWNLOAD_DIR",
            Self::Templates => "XDG_TEMPLATES_DIR",
            Self::PublicShare => "XDG_PUBLICSHARE_DIR",
            Self::Documents => "XDG_DOCUMENTS_DIR",
            Self::Music => "XDG_MUSIC_DIR",
            Self::Pictures => "XDG_PICTURES_DIR",
            Self::Videos => "XDG_VIDEOS_DIR",
        }
    }

    /// The name `xdg-user-dir NAME` and `--set NAME` accept.
    #[must_use]
    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Desktop => "DESKTOP",
            Self::Download => "DOWNLOAD",
            Self::Templates => "TEMPLATES",
            Self::PublicShare => "PUBLICSHARE",
            Self::Documents => "DOCUMENTS",
            Self::Music => "MUSIC",
            Self::Pictures => "PICTURES",
            Self::Videos => "VIDEOS",
        }
    }

    /// Resolve a command-line name, which the tools match case-sensitively.
    #[must_use]
    pub fn from_cli_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|dir| dir.cli_name() == name)
    }

    /// Resolve a `user-dirs.dirs` key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|dir| dir.key() == key)
    }

    /// The spec's fallback, relative to home, when the file says nothing.
    #[must_use]
    pub const fn fallback(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Download => "Downloads",
            Self::Templates => "Templates",
            Self::PublicShare => "Public",
            Self::Documents => "Documents",
            Self::Music => "Music",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
        }
    }

    /// The English name a user interface shows.
    ///
    /// Deliberately *not* the directory's basename: a localized setup has
    /// `XDG_DOWNLOAD_DIR="$HOME/Descargas"`, and a sidebar mixing English and
    /// Spanish labels reads worse than one that is consistently in the
    /// application's own language.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Download => "Downloads",
            Self::Templates => "Templates",
            Self::PublicShare => "Public",
            Self::Documents => "Documents",
            Self::Music => "Music",
            Self::Pictures => "Pictures",
            Self::Videos => "Videos",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UserDir;

    #[test]
    fn every_key_and_name_is_unique() {
        let mut keys: Vec<_> = UserDir::ALL.iter().map(|d| d.key()).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "two directories share a key");

        let mut names: Vec<_> = UserDir::ALL.iter().map(|d| d.cli_name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two directories share a CLI name");
    }

    #[test]
    fn every_directory_round_trips_through_its_key_and_name() {
        for dir in UserDir::ALL {
            assert_eq!(UserDir::from_key(dir.key()), Some(dir));
            assert_eq!(UserDir::from_cli_name(dir.cli_name()), Some(dir));
        }
    }

    /// `xdg-user-dir` matches names exactly; a lowercase argument is not a
    /// directory name and must not silently resolve to one.
    #[test]
    fn a_lowercase_name_is_not_accepted() {
        assert_eq!(UserDir::from_cli_name("desktop"), None);
        assert_eq!(UserDir::from_cli_name(""), None);
        assert_eq!(UserDir::from_cli_name("HOME"), None);
    }

    /// The two names that do not match their key stem are exactly the ones
    /// the spec spells differently, and getting either wrong silently breaks
    /// a real caller.
    #[test]
    fn the_irregular_names_match_the_specification() {
        assert_eq!(UserDir::Download.key(), "XDG_DOWNLOAD_DIR");
        assert_eq!(UserDir::Download.fallback(), "Downloads");
        assert_eq!(UserDir::PublicShare.key(), "XDG_PUBLICSHARE_DIR");
        assert_eq!(UserDir::PublicShare.fallback(), "Public");
    }
}
