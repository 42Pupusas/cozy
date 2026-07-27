//! Discover installed applications by scanning the XDG `applications`
//! directories for `.desktop` files.
//!
//! Search order follows the [XDG Base Directory
//! spec](https://specifications.freedesktop.org/basedir-spec/latest/):
//! `$XDG_DATA_HOME/applications` (default `~/.local/share/applications`)
//! first, then each dir in `$XDG_DATA_DIRS` (default
//! `/usr/local/share:/usr/share`) joined with `/applications`. Earlier dirs
//! win on a filename collision -- a user's local override of a system
//! `.desktop` file shadows the system one, exactly as the spec requires.

use crate::desktop_entry::DesktopEntry;
use std::collections::HashSet;
use std::path::PathBuf;

/// The set of launchable applications found on disk, in first-seen order
/// (i.e. highest-priority directory first, alphabetical within a
/// directory).
pub struct AppIndex {
    pub entries: Vec<DesktopEntry>,
}

impl AppIndex {
    /// Scan the standard XDG `applications` directories and parse every
    /// `.desktop` file found, skipping ones [`DesktopEntry::parse`] rejects
    /// and de-duplicating by filename (basename) so a user override in
    /// `XDG_DATA_HOME` hides the system-wide entry of the same name rather
    /// than both appearing.
    #[must_use]
    pub fn scan() -> Self {
        let mut entries = Vec::new();
        let mut seen_basenames = HashSet::new();

        for dir in Self::search_dirs() {
            let Ok(read_dir) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = read_dir.filter_map(|e| Some(e.ok()?.path())).collect();
            // Alphabetical within a directory so the same machine always
            // produces the same order (readdir order is unspecified).
            paths.sort();

            for path in paths {
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let Some(basename) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !seen_basenames.insert(basename.to_owned()) {
                    continue;
                }
                if let Some(entry) = DesktopEntry::load(&path) {
                    entries.push(entry);
                }
            }
        }

        Self { entries }
    }

    /// Directories to search for `.desktop` files, highest-priority first:
    /// `$XDG_DATA_HOME/applications`, then each `$XDG_DATA_DIRS` entry's
    /// `applications` subdirectory.
    fn search_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();

        let xdg_data_home = std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.local/share")
        });
        dirs.push(PathBuf::from(xdg_data_home).join("applications"));

        let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
        for dir in xdg_data_dirs.split(':').filter(|d| !d.is_empty()) {
            dirs.push(PathBuf::from(dir).join("applications"));
        }

        dirs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Serializes every test in this module that touches process-wide env
    /// vars: `cargo test` runs tests on multiple threads by default, and
    /// `std::env::set_var` has no per-thread scoping -- without this,
    /// concurrent tests would stomp each other's `XDG_DATA_HOME`/
    /// `XDG_DATA_DIRS` mid-scan and see a random mix of both tests' fixture
    /// directories.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Scoped `HOME`/`XDG_DATA_HOME`/`XDG_DATA_DIRS` override for a test:
    /// restores the previous values on drop so tests don't leak env state
    /// into each other or the rest of the process. Holds `ENV_MUTEX` for
    /// its whole lifetime so overlapping tests can't interleave their env
    /// var changes.
    struct EnvGuard {
        vars: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(pairs: &[(&'static str, &str)]) -> Self {
            // A prior test panicking while holding the lock would poison
            // it; recover the guard rather than cascading failures across
            // every other test in this module.
            let lock = ENV_MUTEX
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let vars = pairs
                .iter()
                .map(|(k, v)| {
                    let prev = std::env::var(k).ok();
                    // SAFETY: tests in this module are serialized by
                    // `#[test]`'s default single-process, and this guard's
                    // Drop restores the prior value before the next test's
                    // guard runs -- no other thread reads these vars
                    // concurrently within this crate's test binary.
                    unsafe { std::env::set_var(k, v) };
                    (*k, prev)
                })
                .collect();
            Self { vars, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, prev) in &self.vars {
                match prev {
                    // SAFETY: see `set` above.
                    Some(v) => unsafe { std::env::set_var(k, v) },
                    None => unsafe { std::env::remove_var(k) },
                }
            }
        }
    }

    fn write_desktop_file(dir: &std::path::Path, name: &str, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn scans_and_parses_entries() {
        let tmp = std::env::temp_dir().join(format!("launcher_test_{}", std::process::id()));
        let apps = tmp.join("data_home/applications");
        write_desktop_file(
            &apps,
            "foo.desktop",
            "[Desktop Entry]\nName=Foo\nExec=foo\nType=Application\n",
        );

        let _guard = EnvGuard::set(&[
            ("XDG_DATA_HOME", tmp.join("data_home").to_str().unwrap()),
            ("XDG_DATA_DIRS", tmp.join("nonexistent").to_str().unwrap()),
        ]);

        let index = AppIndex::scan();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].name, "Foo");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn user_override_shadows_system_entry_of_same_basename() {
        let tmp = std::env::temp_dir().join(format!("launcher_test2_{}", std::process::id()));
        let home_apps = tmp.join("data_home/applications");
        let sys_apps = tmp.join("data_dirs/share/applications");
        write_desktop_file(
            &home_apps,
            "app.desktop",
            "[Desktop Entry]\nName=UserVersion\nExec=user\nType=Application\n",
        );
        write_desktop_file(
            &sys_apps,
            "app.desktop",
            "[Desktop Entry]\nName=SystemVersion\nExec=sys\nType=Application\n",
        );

        let _guard = EnvGuard::set(&[
            ("XDG_DATA_HOME", tmp.join("data_home").to_str().unwrap()),
            (
                "XDG_DATA_DIRS",
                tmp.join("data_dirs/share").to_str().unwrap(),
            ),
        ]);

        let index = AppIndex::scan();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].name, "UserVersion");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ignores_non_desktop_files() {
        let tmp = std::env::temp_dir().join(format!("launcher_test3_{}", std::process::id()));
        let apps = tmp.join("data_home/applications");
        write_desktop_file(&apps, "readme.txt", "not a desktop file");

        let _guard = EnvGuard::set(&[
            ("XDG_DATA_HOME", tmp.join("data_home").to_str().unwrap()),
            ("XDG_DATA_DIRS", tmp.join("nonexistent").to_str().unwrap()),
        ]);

        let index = AppIndex::scan();
        assert!(index.entries.is_empty());

        fs::remove_dir_all(&tmp).ok();
    }
}
