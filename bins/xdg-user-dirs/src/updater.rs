//! [`Updater`] -- the `xdg-user-dirs-update` behaviour.
//!
//! # The rules, from the manual page
//!
//! * A missing configuration is created from the defaults.
//! * An existing one gains any directory it does not yet state.
//! * A directory that points nowhere is reset to `$HOME`, because the user
//!   deleted it and most likely does not want it any more.
//! * `--force` inverts that last rule: the directory is recreated instead.

use crate::update_options::UpdateOptions;
use std::path::{Path, PathBuf};
use user_dirs::{ConfigPaths, DirsWriter, UserDir, UserDirs, WriteError};

/// Applies one run's changes to the configuration.
pub struct Updater<'a> {
    paths: &'a ConfigPaths,
    options: &'a UpdateOptions,
}

/// What a run changed, so the caller can report it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpdateOutcome {
    /// Directories reset to `$HOME` because they pointed nowhere.
    pub reset: Vec<UserDir>,
    /// Directories created on disk.
    pub created: Vec<PathBuf>,
}

impl<'a> Updater<'a> {
    /// Prepare a run.
    #[must_use]
    pub const fn new(paths: &'a ConfigPaths, options: &'a UpdateOptions) -> Self {
        Self { paths, options }
    }

    /// Read, update, and write the configuration.
    ///
    /// `--dummy-output` redirects the write and suppresses every directory
    /// creation, which is what makes the tool safe to run in a test.
    pub fn run(&self) -> Result<UpdateOutcome, WriteError> {
        for (_, path) in &self.options.assignments {
            DirsWriter::check_absolute(path)?;
        }

        let mut dirs = UserDirs::load_from(self.paths);
        let mut outcome = UpdateOutcome::default();
        let dry = self.options.dummy_output.is_some();

        for dir in UserDir::ALL {
            self.reconcile(&mut dirs, dir, dry, &mut outcome);
        }
        for (dir, path) in &self.options.assignments {
            dirs.set(*dir, path);
            if !dry {
                Self::create(path, &mut outcome);
            }
        }

        self.options.dummy_output.as_ref().map_or_else(
            || DirsWriter::write(&dirs, self.paths),
            |path| DirsWriter::write_to(&dirs, path),
        )?;
        Ok(outcome)
    }

    /// Bring one directory into line with the rules.
    fn reconcile(
        &self,
        dirs: &mut UserDirs,
        dir: UserDir,
        dry: bool,
        outcome: &mut UpdateOutcome,
    ) {
        let path = dirs.path(dir).to_path_buf();
        if path.is_dir() {
            return;
        }
        // The home directory always exists, so a directory already pointing
        // there is settled -- resetting it again would report a change that
        // did not happen.
        if path == self.paths.home() {
            return;
        }
        if self.options.force {
            if !dry {
                Self::create(&path, outcome);
            }
            return;
        }
        dirs.set(dir, self.paths.home());
        outcome.reset.push(dir);
    }

    /// Create a directory, recording it when it did not already exist.
    fn create(path: &Path, outcome: &mut UpdateOutcome) {
        if path.is_dir() {
            return;
        }
        if std::fs::create_dir_all(path).is_ok() {
            outcome.created.push(path.to_path_buf());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{UpdateOptions, Updater};
    use crate::update_options::Command;
    use std::path::{Path, PathBuf};
    use user_dirs::{ConfigPaths, UserDir, UserDirs};

    /// A scratch home directory, removed on drop.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "xdg-user-dirs-{label}-{}-{unique}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create scratch");
            Self { root }
        }

        fn paths(&self) -> ConfigPaths {
            ConfigPaths::new(&self.root, self.root.join(".config"))
        }

        fn dirs(&self) -> UserDirs {
            UserDirs::load_from(&self.paths())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn options() -> UpdateOptions {
        UpdateOptions {
            command: Command::Update,
            force: false,
            dummy_output: None,
            assignments: Vec::new(),
        }
    }

    #[test]
    fn a_missing_configuration_is_created() {
        let scratch = Scratch::new("create");
        let paths = scratch.paths();
        assert!(!paths.dirs_file().exists());
        Updater::new(&paths, &options()).run().expect("run");
        assert!(paths.dirs_file().is_file());
    }

    /// A directory the user deleted is reset to `$HOME` rather than being
    /// silently recreated.
    #[test]
    fn a_directory_that_points_nowhere_is_reset_to_home() {
        let scratch = Scratch::new("reset");
        let paths = scratch.paths();
        let outcome = Updater::new(&paths, &options()).run().expect("run");
        assert!(outcome.reset.contains(&UserDir::Music));
        assert_eq!(scratch.dirs().path(UserDir::Music), paths.home());
        assert!(!paths.home().join("Music").exists(), "nothing was created");
    }

    /// `--force` inverts the reset rule: the directory is recreated.
    #[test]
    fn force_recreates_instead_of_resetting() {
        let scratch = Scratch::new("force");
        let paths = scratch.paths();
        let forced = UpdateOptions {
            force: true,
            ..options()
        };
        let outcome = Updater::new(&paths, &forced).run().expect("run");
        assert!(outcome.reset.is_empty(), "nothing should be reset");
        assert!(paths.home().join("Music").is_dir());
        assert_eq!(scratch.dirs().path(UserDir::Music), paths.home().join("Music"));
    }

    /// An existing directory is left exactly where it is.
    #[test]
    fn an_existing_directory_is_left_alone() {
        let scratch = Scratch::new("keep");
        let paths = scratch.paths();
        std::fs::create_dir_all(paths.home().join("Music")).expect("create");
        let outcome = Updater::new(&paths, &options()).run().expect("run");
        assert!(!outcome.reset.contains(&UserDir::Music));
        assert_eq!(scratch.dirs().path(UserDir::Music), paths.home().join("Music"));
    }

    #[test]
    fn set_assigns_and_creates_the_directory() {
        let scratch = Scratch::new("set");
        let paths = scratch.paths();
        let target = paths.home().join("Tunes");
        let assigned = UpdateOptions {
            assignments: vec![(UserDir::Music, target.clone())],
            ..options()
        };
        Updater::new(&paths, &assigned).run().expect("run");
        assert_eq!(scratch.dirs().path(UserDir::Music), target);
        assert!(target.is_dir());
    }

    /// The manual page requires an absolute path, and a relative one must be
    /// refused before anything is written.
    #[test]
    fn a_relative_assignment_is_refused_and_writes_nothing() {
        let scratch = Scratch::new("relative");
        let paths = scratch.paths();
        let bad = UpdateOptions {
            assignments: vec![(UserDir::Music, PathBuf::from("relative/path"))],
            ..options()
        };
        assert!(Updater::new(&paths, &bad).run().is_err());
        assert!(
            !paths.dirs_file().exists(),
            "a refused run must not write the configuration"
        );
    }

    /// `--dummy-output` writes elsewhere and creates nothing, which is what
    /// makes the tool safe to run against a real home directory.
    #[test]
    fn dummy_output_writes_elsewhere_and_creates_nothing() {
        let scratch = Scratch::new("dummy");
        let paths = scratch.paths();
        let out = scratch.root.join("preview.dirs");
        let dry = UpdateOptions {
            force: true,
            dummy_output: Some(out.clone()),
            assignments: vec![(UserDir::Music, paths.home().join("Tunes"))],
            ..options()
        };
        Updater::new(&paths, &dry).run().expect("run");
        assert!(out.is_file(), "the preview was written");
        assert!(!paths.dirs_file().exists(), "the real file was untouched");
        assert!(!paths.home().join("Tunes").exists(), "nothing was created");
    }

    /// Running twice must reach the same state -- the session runs this at
    /// every login, and a tool that drifted would relocate directories.
    #[test]
    fn a_second_run_changes_nothing() {
        let scratch = Scratch::new("idempotent");
        let paths = scratch.paths();
        Updater::new(&paths, &options()).run().expect("first");
        let first = std::fs::read_to_string(paths.dirs_file()).expect("read");

        let outcome = Updater::new(&paths, &options()).run().expect("second");
        let second = std::fs::read_to_string(paths.dirs_file()).expect("read");

        assert_eq!(first, second, "the file changed on the second run");
        assert!(outcome.reset.is_empty(), "nothing was left to reset");
    }

    /// A key outside the eight is legal -- this machine's own file carries
    /// `XDG_PROJECTS_DIR` -- and the session runs this tool at every login.
    /// Dropping such a key would quietly delete the user's configuration.
    #[test]
    fn an_unrecognised_key_survives_an_update() {
        let scratch = Scratch::new("unknown-key");
        let paths = scratch.paths();
        std::fs::create_dir_all(paths.config_home()).expect("create config");
        std::fs::write(
            paths.dirs_file(),
            "XDG_PROJECTS_DIR=\"$HOME/Projects\"\nXDG_MUSIC_DIR=\"$HOME/\"\n",
        )
        .expect("write");

        Updater::new(&paths, &options()).run().expect("run");

        let text = std::fs::read_to_string(paths.dirs_file()).expect("read back");
        assert!(
            text.contains("XDG_PROJECTS_DIR=\"$HOME/Projects\""),
            "the key was dropped; got:\n{text}"
        );
    }

    /// A path the user set by hand must survive the next login.
    #[test]
    fn a_hand_written_path_survives_an_update() {
        let scratch = Scratch::new("preserve");
        let paths = scratch.paths();
        let custom = scratch.root.join("elsewhere/Music");
        std::fs::create_dir_all(&custom).expect("create");
        std::fs::create_dir_all(paths.config_home()).expect("create config");
        std::fs::write(
            paths.dirs_file(),
            format!("XDG_MUSIC_DIR=\"{}\"\n", custom.display()),
        )
        .expect("write");

        Updater::new(&paths, &options()).run().expect("run");
        assert_eq!(scratch.dirs().path(UserDir::Music), custom);
    }

    /// A directory already pointing at `$HOME` is settled; reporting it as
    /// reset on every run would be a lie the caller prints.
    #[test]
    fn a_directory_already_at_home_is_not_reported_as_reset() {
        let scratch = Scratch::new("athome");
        let paths = scratch.paths();
        Updater::new(&paths, &options()).run().expect("first");
        let outcome = Updater::new(&paths, &options()).run().expect("second");
        assert!(outcome.reset.is_empty());
        assert_eq!(scratch.dirs().path(UserDir::Music), paths.home());
    }

    /// The written file must be the one the reader understands.
    #[test]
    fn the_written_file_reloads_identically() {
        let scratch = Scratch::new("reload");
        let paths = scratch.paths();
        let target = paths.home().join("My Music");
        let assigned = UpdateOptions {
            assignments: vec![(UserDir::Music, target.clone())],
            ..options()
        };
        Updater::new(&paths, &assigned).run().expect("run");
        assert_eq!(scratch.dirs().path(UserDir::Music), target);
        assert_eq!(
            user_dirs::DirLookup::new(&paths).resolve("MUSIC"),
            target,
            "the lookup tool must agree with the writer"
        );
        assert_ne!(scratch.dirs().path(UserDir::Music), Path::new(""));
    }
}
