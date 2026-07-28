//! Locating the trash directory a given file belongs in, and allocating a
//! collision-free name inside it.
//!
//! # Why this isn't just `~/.local/share/Trash`
//!
//! Trashing is implemented as a **rename**, and `rename(2)` cannot cross
//! filesystem boundaries (`EXDEV`). So a file on a mounted volume cannot go
//! into the home trash; the spec defines a per-volume `$topdir/.Trash-$uid`
//! for exactly this. Getting this wrong means either a hard failure on every
//! external drive, or -- far worse -- silently falling back to a *copy* and
//! leaving the user's data in two places.
//!
//! # Layout
//!
//! ```text
//! <root>/files/<name>            the file itself
//! <root>/info/<name>.trashinfo   where it came from
//! ```
//!
//! Both names share the same `<name>`, which is what lets a restore pair them
//! up. Allocating that name atomically is this module's other job.

use std::io;
use std::path::{Path, PathBuf};

/// A resolved trash directory: the `files/` and `info/` pair under one root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashDir {
    root: PathBuf,
}

impl TrashDir {
    /// The home trash, `$XDG_DATA_HOME/Trash` (default `~/.local/share/Trash`).
    ///
    /// Not created here -- see [`Self::ensure_exists`].
    #[must_use]
    pub fn home() -> Option<Self> {
        let data_home = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map_or_else(
                || {
                    let home = std::env::var("HOME").ok()?;
                    (!home.is_empty()).then(|| PathBuf::from(home).join(".local/share"))
                },
                |s| Some(PathBuf::from(s)),
            )?;
        Some(Self {
            root: data_home.join("Trash"),
        })
    }

    /// Wrap an explicit root -- the seam tests use, so they never touch the
    /// developer's real trash.
    #[must_use]
    pub const fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The trash root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where trashed files live.
    #[must_use]
    pub fn files_dir(&self) -> PathBuf {
        self.root.join("files")
    }

    /// Where `.trashinfo` records live.
    #[must_use]
    pub fn info_dir(&self) -> PathBuf {
        self.root.join("info")
    }

    /// Create `files/` and `info/` if absent.
    ///
    /// The root itself is created with `0o700`: the trash holds whatever the
    /// user deleted, which is frequently more sensitive than what they kept.
    pub fn ensure_exists(&self) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        if !self.root.exists() {
            std::fs::create_dir_all(&self.root)?;
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::create_dir_all(self.files_dir())?;
        std::fs::create_dir_all(self.info_dir())?;
        Ok(())
    }

    /// Whether `path` is on the same filesystem as this trash root.
    ///
    /// Compares `st_dev`. A `false` here is what forces the caller to look
    /// for a volume-local trash instead of attempting a doomed rename.
    #[must_use]
    pub fn is_same_filesystem_as(&self, path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;

        // The root may not exist yet, so walk up to the nearest existing
        // ancestor -- its device is the one a freshly-created root would
        // inherit.
        let Some(root_dev) = Self::nearest_existing_device(&self.root) else {
            return false;
        };
        // `symlink_metadata`: trashing a symlink moves the *link*, so the
        // link's own device is what matters, not its target's.
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        meta.dev() == root_dev
    }

    /// `st_dev` of `path` or, if it doesn't exist yet, of its closest
    /// existing ancestor.
    fn nearest_existing_device(path: &Path) -> Option<u64> {
        use std::os::unix::fs::MetadataExt;

        let mut candidate = Some(path);
        while let Some(current) = candidate {
            if let Ok(meta) = std::fs::metadata(current) {
                return Some(meta.dev());
            }
            candidate = current.parent();
        }
        None
    }

    /// Reserve an unused name for `original_name`, creating the `.trashinfo`
    /// file as the lock.
    ///
    /// Returns `(files_path, info_path)`.
    ///
    /// # Why the info file is the lock
    ///
    /// The spec requires that name allocation be atomic, and the only
    /// portable atomic primitive available is `O_EXCL` file creation. So we
    /// create `info/<name>.trashinfo` exclusively *first*: whoever wins that
    /// race owns the name, and the loser retries with the next suffix. Doing
    /// it the other way round (probe, then rename) is a TOCTOU window in
    /// which two concurrent trash operations can pick the same name and one
    /// silently overwrites the other's file -- data loss.
    ///
    /// Suffixes follow the convention already visible in this machine's
    /// trash (`monitor`, `monitor.1`), inserted *before* the extension so
    /// `notes.txt` becomes `notes.1.txt` and stays openable.
    pub fn reserve_name(&self, original_name: &str) -> io::Result<(PathBuf, PathBuf)> {
        // A trashed entry must never be able to escape the trash directory.
        // `original_name` comes from a user-controlled path, so a `..` or an
        // embedded separator would let a crafted name write outside `files/`.
        let sanitised = Self::sanitise(original_name);

        let files_dir = self.files_dir();
        let info_dir = self.info_dir();

        // Bounded rather than `loop`: if 10k names are taken, something is
        // pathologically wrong and failing beats spinning forever.
        for attempt in 0..10_000u32 {
            let candidate = Self::candidate_name(&sanitised, attempt);
            let info_path = info_dir.join(format!("{candidate}.trashinfo"));
            let files_path = files_dir.join(&candidate);

            // Skip a name whose `files/` entry exists even if its info file
            // doesn't -- an orphaned file from an interrupted operation must
            // not be clobbered.
            if files_path.symlink_metadata().is_ok() {
                continue;
            }

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)
            {
                Ok(_) => return Ok((files_path, info_path)),
                // Lost the race for this name; the loop tries the next.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not find an unused name in the trash after 10000 attempts",
        ))
    }

    /// `name` for attempt 0, then `stem.N.ext`.
    fn candidate_name(name: &str, attempt: u32) -> String {
        if attempt == 0 {
            return name.to_owned();
        }
        // Split on the *first* dot after a non-empty stem so `archive.tar.gz`
        // becomes `archive.1.tar.gz` rather than `archive.tar.1.gz` -- the
        // whole multi-part extension is preserved.
        match name.split_once('.') {
            // A leading dot means a dotfile (`.bashrc`), not an extension, so
            // there's no stem to suffix -- append instead.
            Some((stem, ext)) if !stem.is_empty() => format!("{stem}.{attempt}.{ext}"),
            _ => format!("{name}.{attempt}"),
        }
    }

    /// Reduce a name to something that cannot escape the trash directory.
    ///
    /// Strips path separators and rejects the `.`/`..` specials. A name that
    /// reduces to nothing becomes `trashed`, because an empty filename can't
    /// be created at all.
    fn sanitise(name: &str) -> String {
        let cleaned: String = name.chars().filter(|&c| c != '/' && c != '\0').collect();
        if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
            return "trashed".to_owned();
        }
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn trash_in(tmp: &TempDir) -> TrashDir {
        let dir = TrashDir::at(tmp.join("Trash"));
        dir.ensure_exists().expect("create trash");
        dir
    }

    #[test]
    fn exposes_the_spec_layout() {
        let dir = TrashDir::at(PathBuf::from("/x/Trash"));
        assert_eq!(dir.files_dir(), PathBuf::from("/x/Trash/files"));
        assert_eq!(dir.info_dir(), PathBuf::from("/x/Trash/info"));
    }

    #[test]
    fn ensure_exists_creates_both_subdirectories() {
        let tmp = TempDir::new("ensure");
        let dir = trash_in(&tmp);
        assert!(dir.files_dir().is_dir());
        assert!(dir.info_dir().is_dir());
    }

    #[test]
    fn ensure_exists_is_idempotent() {
        let tmp = TempDir::new("idempotent");
        let dir = trash_in(&tmp);
        dir.ensure_exists().expect("second call must succeed");
        assert!(dir.files_dir().is_dir());
    }

    /// The trash can hold more sensitive data than the live tree, so it must
    /// not be world-readable.
    #[test]
    fn the_root_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new("perms");
        let dir = trash_in(&tmp);
        let mode = std::fs::metadata(dir.root())
            .expect("stat root")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "trash root must be 0700, got {mode:o}");
    }

    #[test]
    fn home_trash_follows_xdg_data_home() {
        // Not asserting against the real environment (which varies); just
        // that the path ends in the spec's directory name.
        if let Some(dir) = TrashDir::home() {
            assert!(
                dir.root().ends_with("Trash"),
                "got {}",
                dir.root().display()
            );
        }
    }

    #[test]
    fn first_reservation_uses_the_original_name() {
        let tmp = TempDir::new("reserve-first");
        let dir = trash_in(&tmp);

        let (files, info) = dir.reserve_name("notes.txt").expect("reserve");
        assert_eq!(files.file_name().unwrap(), "notes.txt");
        assert_eq!(info.file_name().unwrap(), "notes.txt.trashinfo");
        assert!(info.is_file(), "the info file is created as the lock");
    }

    /// The convention this machine's own trash already uses (`monitor`,
    /// `monitor.1`).
    #[test]
    fn a_taken_name_gets_a_numeric_suffix() {
        let tmp = TempDir::new("reserve-collide");
        let dir = trash_in(&tmp);

        let (first, _) = dir.reserve_name("monitor").expect("first");
        let (second, _) = dir.reserve_name("monitor").expect("second");

        assert_eq!(first.file_name().unwrap(), "monitor");
        assert_eq!(second.file_name().unwrap(), "monitor.1");
    }

    #[test]
    fn suffixes_go_before_the_extension_so_files_stay_openable() {
        let tmp = TempDir::new("reserve-ext");
        let dir = trash_in(&tmp);

        dir.reserve_name("notes.txt").expect("first");
        let (second, info) = dir.reserve_name("notes.txt").expect("second");
        assert_eq!(second.file_name().unwrap(), "notes.1.txt");
        assert_eq!(info.file_name().unwrap(), "notes.1.txt.trashinfo");
    }

    #[test]
    fn multi_part_extensions_are_kept_whole() {
        let tmp = TempDir::new("reserve-tar");
        let dir = trash_in(&tmp);

        dir.reserve_name("archive.tar.gz").expect("first");
        let (second, _) = dir.reserve_name("archive.tar.gz").expect("second");
        assert_eq!(second.file_name().unwrap(), "archive.1.tar.gz");
    }

    #[test]
    fn dotfiles_get_the_suffix_appended_not_inserted() {
        let tmp = TempDir::new("reserve-dotfile");
        let dir = trash_in(&tmp);

        dir.reserve_name(".bashrc").expect("first");
        let (second, _) = dir.reserve_name(".bashrc").expect("second");
        // A leading dot is not an extension separator.
        assert_eq!(second.file_name().unwrap(), ".bashrc.1");
    }

    #[test]
    fn many_collisions_keep_producing_distinct_names() {
        let tmp = TempDir::new("reserve-many");
        let dir = trash_in(&tmp);

        let mut seen = std::collections::HashSet::new();
        for _ in 0..25 {
            let (files, _) = dir.reserve_name("dup.txt").expect("reserve");
            assert!(
                seen.insert(files.file_name().unwrap().to_owned()),
                "duplicate name allocated: {}",
                files.display()
            );
        }
    }

    /// An interrupted trash could leave a file with no info record. Reusing
    /// that name would overwrite it.
    #[test]
    fn an_orphaned_file_without_an_info_record_is_not_clobbered() {
        let tmp = TempDir::new("reserve-orphan");
        let dir = trash_in(&tmp);

        std::fs::write(dir.files_dir().join("orphan"), b"precious").expect("write orphan");
        let (files, _) = dir.reserve_name("orphan").expect("reserve");

        assert_ne!(files.file_name().unwrap(), "orphan");
        assert_eq!(
            std::fs::read(dir.files_dir().join("orphan")).expect("still there"),
            b"precious",
            "the orphan must be left untouched"
        );
    }

    /// The security case: a crafted name must not write outside `files/`.
    #[test]
    fn path_separators_cannot_escape_the_trash() {
        let tmp = TempDir::new("reserve-escape");
        let dir = trash_in(&tmp);

        let (files, info) = dir.reserve_name("../../escaped").expect("reserve");
        assert!(
            files.starts_with(dir.files_dir()),
            "{} escaped {}",
            files.display(),
            dir.files_dir().display()
        );
        assert!(info.starts_with(dir.info_dir()));
        // `..` and `/` are stripped, leaving a plain name.
        assert_eq!(files.file_name().unwrap(), "....escaped");
    }

    #[test]
    fn degenerate_names_become_a_placeholder() {
        let tmp = TempDir::new("reserve-degenerate");
        let dir = trash_in(&tmp);

        for name in ["", ".", ".."] {
            let (files, _) = dir.reserve_name(name).expect("reserve");
            assert!(files.starts_with(dir.files_dir()));
            let allocated = files.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                allocated.starts_with("trashed"),
                "{name:?} should become a placeholder, got {allocated}"
            );
        }
    }

    #[test]
    fn a_file_in_the_same_temp_dir_is_on_the_same_filesystem() {
        let tmp = TempDir::new("samefs");
        let dir = trash_in(&tmp);
        let file = tmp.create_file("thing.txt", b"x");

        assert!(
            dir.is_same_filesystem_as(&file),
            "a file and a trash root under the same temp dir share a device"
        );
    }

    #[test]
    fn a_missing_file_is_not_reported_as_same_filesystem() {
        let tmp = TempDir::new("samefs-missing");
        let dir = trash_in(&tmp);
        assert!(!dir.is_same_filesystem_as(&tmp.join("absent")));
    }

    /// The device check must work before the trash root exists, since
    /// resolution happens before creation.
    #[test]
    fn device_comparison_works_for_a_not_yet_created_root() {
        let tmp = TempDir::new("samefs-uncreated");
        let dir = TrashDir::at(tmp.join("Trash/not/created/yet"));
        let file = tmp.create_file("thing.txt", b"x");
        assert!(dir.is_same_filesystem_as(&file));
    }

    /// `/proc` is always a different filesystem from `/tmp`, making this a
    /// real cross-device assertion without needing a mount.
    #[test]
    fn a_file_on_another_filesystem_is_detected() {
        let tmp = TempDir::new("crossfs");
        let dir = trash_in(&tmp);

        let procfile = Path::new("/proc/self/status");
        if !procfile.exists() {
            return; // no procfs here; nothing to assert against
        }
        assert!(
            !dir.is_same_filesystem_as(procfile),
            "/proc must not be reported as the same device as /tmp"
        );
    }
}
