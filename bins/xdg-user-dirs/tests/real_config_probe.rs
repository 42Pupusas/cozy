//! Reads this machine's real `user-dirs.dirs`, if it has one.
//!
//! # Why a probe rather than a fixture
//!
//! Every test beside this one runs against a scratch directory, which means
//! they all agree with each other about the format and none of them can tell
//! whether that shared belief matches reality.
//!
//! This one reads the file the installed `xdg-user-dirs-update` wrote. It
//! caught a real fault: the writer dropped `XDG_PROJECTS_DIR`, a key outside
//! the eight that this machine's own configuration carries.
//!
//! It skips when there is no file, so a container or a fresh account does
//! not fail the suite.

use user_dirs::{ConfigPaths, DirLookup, DirsWriter, UserDir, UserDirs};

/// Reads the real configuration, and reports when there is none.
struct RealConfig {
    paths: ConfigPaths,
}

impl RealConfig {
    /// The machine's own configuration, if the file exists.
    fn find() -> Option<Self> {
        let paths = ConfigPaths::from_env();
        paths.dirs_file().is_file().then_some(Self { paths })
    }

    fn contents(&self) -> String {
        std::fs::read_to_string(self.paths.dirs_file()).expect("read the real configuration")
    }

    fn dirs(&self) -> UserDirs {
        UserDirs::load_from(&self.paths)
    }
}

/// Rewriting the real file must not lose a line.
///
/// Nothing is written to disk: the rendered text is compared in memory.
#[test]
fn rewriting_the_real_configuration_preserves_every_key() {
    let Some(real) = RealConfig::find() else {
        return;
    };
    let original = real.contents();
    let rendered = DirsWriter::render(&real.dirs());

    for line in original.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        assert!(
            rendered.contains(key.trim_end()),
            "rewriting would drop {key}\n\noriginal:\n{original}\nrendered:\n{rendered}"
        );
    }
}

/// Every path the real file states must survive a rewrite unchanged.
#[test]
fn every_real_path_survives_a_rewrite() {
    let Some(real) = RealConfig::find() else {
        return;
    };
    let before = real.dirs();
    let after = UserDirs::parse(&DirsWriter::render(&before), before.home());
    for dir in UserDir::ALL {
        assert_eq!(
            after.path(dir),
            before.path(dir),
            "{} changed across a rewrite",
            dir.key()
        );
    }
}

/// The two tools must agree about the real file.
///
/// They use different parsers on purpose -- `DirLookup` follows the
/// reference implementation's stricter rules -- so a disagreement here is a
/// genuine incompatibility rather than a duplicated bug.
#[test]
fn the_lookup_agrees_with_the_reader_on_the_real_file() {
    let Some(real) = RealConfig::find() else {
        return;
    };
    let dirs = real.dirs();
    let lookup = DirLookup::new(&real.paths);
    let contents = real.contents();

    for dir in UserDir::ALL {
        // Only keys the file actually states are comparable: the two
        // deliberately disagree about fallbacks, which is the whole reason
        // `DirLookup` exists.
        if !contents.contains(dir.key()) {
            continue;
        }
        assert_eq!(
            lookup.resolve(dir.cli_name()),
            dirs.path(dir),
            "the tools disagree about {}",
            dir.key()
        );
    }
}
