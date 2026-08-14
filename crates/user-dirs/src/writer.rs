//! [`DirsWriter`] -- writing `user-dirs.dirs`.
//!
//! # Why the write is atomic
//!
//! Every desktop application reads this file at startup. A partial write
//! leaves a truncated line, which parses as a missing key, which silently
//! relocates the user's Documents to the fallback. Writing a temporary file
//! and renaming it means a reader sees either the old file or the new one.

use crate::config_paths::ConfigPaths;
use crate::user_dir::UserDir;
use crate::user_dirs::UserDirs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A failure while writing the configuration.
#[derive(Debug)]
pub enum WriteError {
    /// The value is not an absolute path, which the spec requires.
    NotAbsolute(PathBuf),
    /// The filesystem refused the write.
    Io(std::io::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAbsolute(path) => {
                write!(f, "path must be absolute: {}", path.display())
            }
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<std::io::Error> for WriteError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Serializes [`UserDirs`] back to the `user-dirs.dirs` format.
pub struct DirsWriter;

impl DirsWriter {
    /// The header `xdg-user-dirs-update` writes, kept so the file still
    /// warns anyone who opens it.
    const HEADER: &'static str = "\
# This file is written by xdg-user-dirs-update
# If you want to change or add directories, just edit the line you're
# interested in. All local changes will be retained on the next run.
# Format is XDG_xxx_DIR=\"$HOME/yyy\", where yyy is a shell-escaped
# homedir-relative path, or XDG_xxx_DIR=\"/yyy\", where /yyy is an
# absolute path. No other format is supported.
#
";

    /// Render the file's contents.
    ///
    /// Values under the home directory are written `$HOME`-relative, which
    /// is what the real tool does and what keeps a home directory movable.
    #[must_use]
    pub fn render(dirs: &UserDirs) -> String {
        let mut out = String::from(Self::HEADER);
        for dir in UserDir::ALL {
            let value = Self::render_value(dirs.path(dir), dirs.home());
            out.push_str(dir.key());
            out.push_str("=\"");
            out.push_str(&value);
            out.push_str("\"\n");
        }
        out
    }

    /// One value, `$HOME`-relative where possible and shell-escaped.
    fn render_value(path: &Path, home: &Path) -> String {
        path.strip_prefix(home).map_or_else(
            |_| Self::escape(&path.to_string_lossy()),
            |rest| {
                let rest = rest.to_string_lossy();
                if rest.is_empty() {
                    "$HOME/".to_owned()
                } else {
                    format!("$HOME/{}", Self::escape(&rest))
                }
            },
        )
    }

    /// Escape the characters that would otherwise change meaning inside the
    /// double quotes the format uses.
    ///
    /// A directory named `My "Music"` is legal on disk, and writing it
    /// unescaped would produce a line that parses as a different path.
    fn escape(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for ch in value.chars() {
            if matches!(ch, '"' | '\\' | '$' | '`') {
                out.push('\\');
            }
            out.push(ch);
        }
        out
    }

    /// Write the configuration to `path`, atomically.
    pub fn write_to(dirs: &UserDirs, path: &Path) -> Result<(), WriteError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = Self::temp_path(path);
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(Self::render(dirs).as_bytes())?;
        // The rename publishes the file, so the bytes must be on the disk
        // before it happens; otherwise a crash can leave an empty file where
        // a valid one used to be.
        file.sync_all()?;
        drop(file);
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                Err(WriteError::Io(error))
            }
        }
    }

    /// Write to the standard location.
    pub fn write(dirs: &UserDirs, paths: &ConfigPaths) -> Result<(), WriteError> {
        Self::write_to(dirs, &paths.dirs_file())
    }

    /// A sibling temporary path, so the rename stays on one filesystem.
    fn temp_path(path: &Path) -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".tmp.{}", std::process::id()));
        path.with_file_name(name)
    }

    /// Reject a value the spec does not allow, before anything is written.
    pub fn check_absolute(path: &Path) -> Result<(), WriteError> {
        if path.is_absolute() {
            Ok(())
        } else {
            Err(WriteError::NotAbsolute(path.to_path_buf()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirsWriter, WriteError};
    use crate::user_dir::UserDir;
    use crate::user_dirs::UserDirs;
    use std::path::{Path, PathBuf};

    fn home() -> PathBuf {
        PathBuf::from("/home/tester")
    }

    #[test]
    fn every_directory_is_written() {
        let dirs = UserDirs::parse("", &home());
        let text = DirsWriter::render(&dirs);
        for dir in UserDir::ALL {
            assert!(text.contains(dir.key()), "{} is missing", dir.key());
        }
    }

    #[test]
    fn a_home_relative_path_is_written_back_as_home_relative() {
        let dirs = UserDirs::parse("", &home());
        let text = DirsWriter::render(&dirs);
        assert!(text.contains(r#"XDG_DOCUMENTS_DIR="$HOME/Documents""#));
    }

    #[test]
    fn a_path_outside_home_is_written_absolute() {
        let mut dirs = UserDirs::parse("", &home());
        dirs.set(UserDir::Music, "/mnt/media/music");
        let text = DirsWriter::render(&dirs);
        assert!(text.contains(r#"XDG_MUSIC_DIR="/mnt/media/music""#));
    }

    /// Home itself is the documented way to disable a directory, and it must
    /// survive a round trip rather than becoming a fallback path.
    #[test]
    fn a_directory_set_to_home_round_trips() {
        let mut dirs = UserDirs::parse("", &home());
        dirs.set(UserDir::Desktop, home());
        let text = DirsWriter::render(&dirs);
        assert!(text.contains(r#"XDG_DESKTOP_DIR="$HOME/""#));
        let reparsed = UserDirs::parse(&text, &home());
        assert_eq!(reparsed.path(UserDir::Desktop), home());
    }

    /// The whole file must survive a round trip, or the update tool would
    /// corrupt the configuration a little on every run.
    #[test]
    fn rendering_then_parsing_preserves_every_path() {
        let mut dirs = UserDirs::parse("", &home());
        dirs.set(UserDir::Music, "/mnt/media/music");
        dirs.set(UserDir::Download, "/home/tester/Descargas");
        let reparsed = UserDirs::parse(&DirsWriter::render(&dirs), &home());
        for dir in UserDir::ALL {
            assert_eq!(reparsed.path(dir), dirs.path(dir), "{} changed", dir.key());
        }
    }

    /// A quote in a directory name must not be able to end the quoted value
    /// and turn the rest of the name into something else.
    #[test]
    fn a_quote_in_a_name_is_escaped() {
        let mut dirs = UserDirs::parse("", &home());
        dirs.set(UserDir::Music, r#"/mnt/My "Music""#);
        let text = DirsWriter::render(&dirs);
        assert!(text.contains(r#"\"Music\""#), "got: {text}");
    }

    /// Escaping is only half a format: what the writer escapes, the parser
    /// must unescape, or a name containing a quote comes back different from
    /// the one that went in.
    #[test]
    fn an_escaped_name_survives_a_round_trip() {
        let awkward = [
            r#"/mnt/My "Music""#,
            r"/mnt/back\slash",
            "/mnt/cost $5",
            "/mnt/tick`mark",
        ];
        for original in awkward {
            let mut dirs = UserDirs::parse("", &home());
            dirs.set(UserDir::Music, original);
            let reparsed = UserDirs::parse(&DirsWriter::render(&dirs), &home());
            assert_eq!(
                reparsed.path(UserDir::Music),
                Path::new(original),
                "{original} did not survive"
            );
        }
    }

    /// The write must land on disk and leave no temporary file behind.
    #[test]
    fn writing_produces_a_readable_file_and_no_leftovers() {
        let root = std::env::temp_dir().join(format!("user-dirs-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = crate::ConfigPaths::new(&root, root.join(".config"));

        let mut dirs = UserDirs::parse("", paths.home());
        dirs.set(UserDir::Music, root.join("Tunes"));
        DirsWriter::write(&dirs, &paths).expect("write");

        let reloaded = UserDirs::load_from(&paths);
        assert_eq!(reloaded.path(UserDir::Music), root.join("Tunes"));

        let strays: Vec<_> = std::fs::read_dir(paths.config_home())
            .expect("read config dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp."))
            .collect();
        assert!(strays.is_empty(), "temporary file left behind: {strays:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Writing over an existing file replaces it rather than appending.
    #[test]
    fn a_second_write_replaces_the_first() {
        let root = std::env::temp_dir().join(format!("user-dirs-twice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = crate::ConfigPaths::new(&root, root.join(".config"));

        let mut dirs = UserDirs::parse("", paths.home());
        dirs.set(UserDir::Music, root.join("First"));
        DirsWriter::write(&dirs, &paths).expect("first write");
        dirs.set(UserDir::Music, root.join("Second"));
        DirsWriter::write(&dirs, &paths).expect("second write");

        let text = std::fs::read_to_string(paths.dirs_file()).expect("read back");
        assert_eq!(text.matches("XDG_MUSIC_DIR").count(), 1);
        assert_eq!(
            UserDirs::load_from(&paths).path(UserDir::Music),
            root.join("Second")
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_relative_path_is_refused() {
        let error = DirsWriter::check_absolute(Path::new("relative/path"));
        assert!(matches!(error, Err(WriteError::NotAbsolute(_))));
        assert!(DirsWriter::check_absolute(Path::new("/absolute")).is_ok());
    }
}
