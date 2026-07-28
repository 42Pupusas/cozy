//! Moving files into the trash, and restoring them back out.
//!
//! Trashing is a **rename**, never a copy: it must be atomic and it must not
//! duplicate the user's data. That single constraint drives the whole design
//! here -- `rename(2)` can't cross filesystems, so a file on another volume
//! needs a volume-local trash (see [`crate::trash_dir`]), and if no such trash
//! is usable the operation *fails* rather than silently degrading to a copy
//! that leaves the data in two places.
//!
//! # Ordering
//!
//! The `.trashinfo` record is written **before** the file moves, because it
//! doubles as the atomic name reservation. That ordering also decides how
//! failures look:
//!
//! * info written, rename fails → a stray info file, and the user's file is
//!   still exactly where it was. We clean the stray up. **Nothing is lost.**
//! * info written, rename succeeds → complete.
//!
//! The reverse order would allow a file to sit in `files/` with no record of
//! where it came from -- unrestorable, which is a form of data loss.

use crate::trash_dir::TrashDir;
use crate::trash_info::TrashInfo;
use localtime::LocalTime;
use std::io;
use std::path::{Path, PathBuf};

/// Why a trash operation failed.
///
/// Distinct variants rather than a bare `io::Error` because the UI must say
/// different things: a missing file is a stale view needing a refresh, whereas
/// "no usable trash on this volume" is a real decision point where the only
/// alternative is permanent deletion.
#[derive(Debug)]
pub enum TrashError {
    /// The source path doesn't exist.
    NotFound(PathBuf),
    /// Refusing to trash a filesystem root or a path with no file name.
    NotTrashable(PathBuf),
    /// No trash directory could be located or created for this path -- most
    /// often a removable volume that isn't writable.
    ///
    /// The caller must **not** fall back to copying; offer permanent deletion
    /// instead, explicitly.
    NoTrashAvailable(PathBuf),
    /// The filesystem refused an operation.
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for TrashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "{} does not exist", p.display()),
            Self::NotTrashable(p) => write!(f, "{} cannot be trashed", p.display()),
            Self::NoTrashAvailable(p) => {
                write!(f, "no trash directory available for {}", p.display())
            }
            Self::Io(p, e) => write!(f, "{}: {e}", p.display()),
        }
    }
}

impl std::error::Error for TrashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(_, e) => Some(e),
            _ => None,
        }
    }
}

/// A file successfully moved to the trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trashed {
    /// Where the file now lives, under `files/`.
    pub trashed_path: PathBuf,
    /// Its `.trashinfo` record.
    pub info_path: PathBuf,
    /// Where it came from.
    pub original_path: PathBuf,
}

/// The trash operation.
///
/// Holds the resolved [`TrashDir`] so a multi-file delete resolves the
/// directory once rather than per file.
#[derive(Debug, Clone)]
pub struct Trash {
    dir: TrashDir,
}

impl Trash {
    /// Use the user's home trash.
    ///
    /// # Errors
    /// [`TrashError::NoTrashAvailable`] if neither `XDG_DATA_HOME` nor `HOME`
    /// is set, leaving nowhere to put it.
    pub fn home() -> Result<Self, TrashError> {
        TrashDir::home()
            .map(|dir| Self { dir })
            .ok_or_else(|| TrashError::NoTrashAvailable(PathBuf::from("$HOME")))
    }

    /// Use an explicit trash directory -- the seam tests use so they never
    /// touch the developer's real trash.
    #[must_use]
    pub const fn at(dir: TrashDir) -> Self {
        Self { dir }
    }

    /// The trash directory in use.
    #[must_use]
    pub const fn dir(&self) -> &TrashDir {
        &self.dir
    }

    /// Move `path` into the trash.
    ///
    /// Works on directories as well as files: a rename moves a whole tree in
    /// one atomic step, which is precisely why trashing a huge folder is
    /// instant while *copying* it isn't.
    ///
    /// # Errors
    /// See [`TrashError`]. On any failure the source file is left untouched.
    pub fn trash(&self, path: &Path) -> Result<Trashed, TrashError> {
        // `symlink_metadata`, not `metadata`: trashing a symlink must move the
        // link itself. Following it would trash the *target* -- deleting a
        // file the user never selected.
        let Ok(_meta) = std::fs::symlink_metadata(path) else {
            return Err(TrashError::NotFound(path.to_path_buf()));
        };

        // Absolute, because the `.trashinfo` `Path` must be absolute for a
        // restore to work from any working directory. Done without
        // `canonicalize`, which resolves symlinks and would record the
        // target's path rather than the link's.
        let original =
            Self::absolutise(path).ok_or_else(|| TrashError::NotTrashable(path.to_path_buf()))?;

        let name = original
            .file_name()
            .and_then(|n| n.to_str().map(str::to_owned))
            // A path with no file name is a filesystem root; refusing beats
            // whatever a rename of `/` would do.
            .ok_or_else(|| TrashError::NotTrashable(original.clone()))?;

        self.dir
            .ensure_exists()
            .map_err(|_| TrashError::NoTrashAvailable(original.clone()))?;

        // Guard the cross-device case *before* touching anything. A rename
        // across filesystems fails with EXDEV, and the only correct
        // alternatives are a volume-local trash or an explicit refusal --
        // never a silent copy.
        if !self.dir.is_same_filesystem_as(&original) {
            return Err(TrashError::NoTrashAvailable(original));
        }

        // Reserving the name creates the info file, which is the lock.
        let (files_path, info_path) = self
            .dir
            .reserve_name(&name)
            .map_err(|e| TrashError::Io(original.clone(), e))?;

        let info = TrashInfo::new(
            original.clone(),
            LocalTime::now().unwrap_or(localtime::Timestamp {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
        );

        if let Err(e) = std::fs::write(&info_path, info.to_file_contents()) {
            // Roll back the reservation so a failed attempt doesn't leave a
            // phantom record pointing at a file that never moved.
            let _ = std::fs::remove_file(&info_path);
            return Err(TrashError::Io(info_path, e));
        }

        if let Err(e) = std::fs::rename(&original, &files_path) {
            // The file is still in place; drop the record we just wrote so
            // the trash doesn't list something that isn't there.
            let _ = std::fs::remove_file(&info_path);
            return Err(TrashError::Io(original, e));
        }

        Ok(Trashed {
            trashed_path: files_path,
            info_path,
            original_path: original,
        })
    }

    /// Restore a trashed entry to where it came from.
    ///
    /// # Errors
    /// [`TrashError::Io`] with [`io::ErrorKind::AlreadyExists`] if something
    /// now occupies the original path. Restoring is never allowed to
    /// overwrite: the file at the destination is one the user still has, and
    /// clobbering it would turn an undo into a deletion.
    pub fn restore(&self, info_path: &Path) -> Result<PathBuf, TrashError> {
        let contents = std::fs::read_to_string(info_path)
            .map_err(|e| TrashError::Io(info_path.to_path_buf(), e))?;
        let info = TrashInfo::parse(&contents)
            .ok_or_else(|| TrashError::NotTrashable(info_path.to_path_buf()))?;

        // The `files/` entry shares the info file's name, minus `.trashinfo`.
        let stem = info_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".trashinfo"))
            .ok_or_else(|| TrashError::NotTrashable(info_path.to_path_buf()))?;
        let files_path = self.dir.files_dir().join(stem);

        if std::fs::symlink_metadata(&files_path).is_err() {
            return Err(TrashError::NotFound(files_path));
        }
        // Refuse to overwrite whatever is at the destination now.
        if std::fs::symlink_metadata(&info.original_path).is_ok() {
            return Err(TrashError::Io(
                info.original_path,
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "something already exists at the original path",
                ),
            ));
        }
        // Recreate the parent if the original directory has since been
        // removed -- otherwise a perfectly restorable file is stuck.
        if let Some(parent) = info.original_path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent).map_err(|e| TrashError::Io(parent.to_path_buf(), e))?;
        }

        std::fs::rename(&files_path, &info.original_path)
            .map_err(|e| TrashError::Io(info.original_path.clone(), e))?;
        // Only drop the record once the file is safely back.
        let _ = std::fs::remove_file(info_path);

        Ok(info.original_path)
    }

    /// Make `path` absolute without resolving symlinks.
    ///
    /// `canonicalize` is deliberately avoided: it would follow a trashed
    /// symlink and record its *target*, so restoring would put the link back
    /// in the wrong place.
    fn absolutise(path: &Path) -> Option<PathBuf> {
        if path.is_absolute() {
            return Some(Self::lexically_normalise(path));
        }
        let cwd = std::env::current_dir().ok()?;
        Some(Self::lexically_normalise(&cwd.join(path)))
    }

    /// Remove `.` and resolve `..` textually, without touching the disk.
    fn lexically_normalise(path: &Path) -> PathBuf {
        use std::path::Component;

        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    // Pop only if there's something to pop; `/..` is `/`.
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// A trash rooted inside `tmp`, so it's on the same filesystem as the
    /// test's files and never touches the developer's real trash.
    fn trash_in(tmp: &TempDir) -> Trash {
        Trash::at(TrashDir::at(tmp.join("Trash")))
    }

    #[test]
    fn moves_a_file_out_of_its_directory() {
        let tmp = TempDir::new("trash-file");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("notes.txt", b"hello");

        let trashed = trash.trash(&file).expect("trash");

        assert!(!file.exists(), "the original must be gone");
        assert!(trashed.trashed_path.is_file(), "it must be in the trash");
        assert_eq!(
            std::fs::read(&trashed.trashed_path).expect("read"),
            b"hello",
            "contents must survive"
        );
    }

    #[test]
    fn writes_a_matching_info_record() {
        let tmp = TempDir::new("trash-info");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("notes.txt", b"x");

        let trashed = trash.trash(&file).expect("trash");
        let text = std::fs::read_to_string(&trashed.info_path).expect("read info");
        let info = TrashInfo::parse(&text).expect("parse");

        assert_eq!(info.original_path, file, "must record where it came from");
        assert!(text.starts_with("[Trash Info]\n"));
        assert_eq!(
            trashed.info_path.file_name().unwrap(),
            "notes.txt.trashinfo",
            "the record pairs with the file by name"
        );
    }

    #[test]
    fn trashes_a_whole_directory_tree_in_one_step() {
        let tmp = TempDir::new("trash-tree");
        let trash = trash_in(&tmp);
        tmp.create_dir("project/src");
        tmp.create_file("project/src/main.rs", b"fn main() {}");

        let dir = tmp.join("project");
        let trashed = trash.trash(&dir).expect("trash dir");

        assert!(!dir.exists());
        assert!(
            trashed.trashed_path.join("src/main.rs").is_file(),
            "the tree must move intact"
        );
    }

    /// Trashing a symlink must move the *link*, never follow it -- following
    /// would delete a file the user never selected.
    #[test]
    fn trashing_a_symlink_moves_the_link_not_its_target() {
        let tmp = TempDir::new("trash-symlink");
        let trash = trash_in(&tmp);
        let target = tmp.create_file("target.txt", b"precious");
        let link = tmp.create_symlink("link.txt", "target.txt");

        let trashed = trash.trash(&link).expect("trash link");

        assert!(target.is_file(), "the target must be untouched");
        assert_eq!(std::fs::read(&target).expect("read"), b"precious");
        assert!(
            std::fs::symlink_metadata(&trashed.trashed_path)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "what landed in the trash must still be a symlink"
        );
    }

    #[test]
    fn a_broken_symlink_can_still_be_trashed() {
        let tmp = TempDir::new("trash-broken");
        let trash = trash_in(&tmp);
        let link = tmp.create_symlink("dangling", "nowhere-at-all");

        let trashed = trash.trash(&link).expect("broken links are trashable");
        assert!(std::fs::symlink_metadata(&trashed.trashed_path).is_ok());
        assert!(std::fs::symlink_metadata(&link).is_err());
    }

    #[test]
    fn creates_the_trash_directory_on_first_use() {
        let tmp = TempDir::new("trash-create");
        let trash = trash_in(&tmp);
        assert!(!trash.dir().root().exists(), "precondition");

        let file = tmp.create_file("a.txt", b"x");
        trash.trash(&file).expect("trash");

        assert!(trash.dir().files_dir().is_dir());
        assert!(trash.dir().info_dir().is_dir());
    }

    #[test]
    fn two_files_of_the_same_name_both_survive() {
        let tmp = TempDir::new("trash-collide");
        let trash = trash_in(&tmp);

        let first = tmp.create_file("a/dup.txt", b"first");
        let second = tmp.create_file("b/dup.txt", b"second");

        let one = trash.trash(&first).expect("first");
        let two = trash.trash(&second).expect("second");

        assert_ne!(one.trashed_path, two.trashed_path, "names must not collide");
        assert_eq!(std::fs::read(&one.trashed_path).expect("read"), b"first");
        assert_eq!(
            std::fs::read(&two.trashed_path).expect("read"),
            b"second",
            "the second must not have overwritten the first"
        );

        // Both records must survive and point at their own origin.
        let info_one = TrashInfo::parse(&std::fs::read_to_string(&one.info_path).unwrap()).unwrap();
        let info_two = TrashInfo::parse(&std::fs::read_to_string(&two.info_path).unwrap()).unwrap();
        assert_eq!(info_one.original_path, first);
        assert_eq!(info_two.original_path, second);
    }

    #[test]
    fn a_relative_path_is_recorded_absolutely() {
        let tmp = TempDir::new("trash-relative");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("rel.txt", b"x");

        // Build a path with `.` and `..` in it that still resolves to `file`.
        let awkward = tmp.join("sub/../rel.txt");
        tmp.create_dir("sub");

        let trashed = trash.trash(&awkward).expect("trash");
        assert_eq!(
            trashed.original_path, file,
            "`..` must be normalised away so a restore lands correctly"
        );
        assert!(trashed.original_path.is_absolute());
    }

    #[test]
    fn a_missing_file_reports_not_found() {
        let tmp = TempDir::new("trash-missing");
        let trash = trash_in(&tmp);

        match trash.trash(&tmp.join("nope.txt")) {
            Err(TrashError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn the_filesystem_root_is_refused() {
        let tmp = TempDir::new("trash-root");
        let trash = trash_in(&tmp);

        match trash.trash(Path::new("/")) {
            Err(TrashError::NotTrashable(_)) => {}
            other => panic!("expected NotTrashable for /, got {other:?}"),
        }
    }

    /// The rule that prevents silent data duplication: a cross-device trash
    /// must fail loudly, not fall back to a copy.
    #[test]
    fn a_cross_filesystem_trash_is_refused_rather_than_copied() {
        let tmp = TempDir::new("trash-crossfs");
        let trash = trash_in(&tmp);

        let procfile = Path::new("/proc/self/status");
        if !procfile.exists() {
            return; // no procfs; nothing to assert
        }
        match trash.trash(procfile) {
            Err(TrashError::NoTrashAvailable(_)) => {}
            other => panic!("expected NoTrashAvailable across devices, got {other:?}"),
        }
        assert!(procfile.exists(), "the source must be untouched");
    }

    /// A failed trash must leave no phantom record behind.
    #[test]
    fn a_failed_trash_leaves_no_orphan_info_file() {
        let tmp = TempDir::new("trash-orphan");
        let trash = trash_in(&tmp);
        // Force the trash to exist so we can count its records.
        let seed = tmp.create_file("seed.txt", b"x");
        trash.trash(&seed).expect("seed");

        let before = std::fs::read_dir(trash.dir().info_dir())
            .expect("read info dir")
            .count();

        let procfile = Path::new("/proc/self/status");
        if procfile.exists() {
            let _ = trash.trash(procfile);
        }

        let after = std::fs::read_dir(trash.dir().info_dir())
            .expect("read info dir")
            .count();
        assert_eq!(before, after, "a failed trash must not leave a record");
    }

    // --- restore ---

    #[test]
    fn restores_a_file_to_where_it_came_from() {
        let tmp = TempDir::new("restore");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("notes.txt", b"hello");

        let trashed = trash.trash(&file).expect("trash");
        let restored = trash.restore(&trashed.info_path).expect("restore");

        assert_eq!(restored, file);
        assert!(file.is_file(), "the file must be back");
        assert_eq!(std::fs::read(&file).expect("read"), b"hello");
        assert!(
            !trashed.trashed_path.exists(),
            "it must no longer be in the trash"
        );
        assert!(
            !trashed.info_path.exists(),
            "its record must be cleaned up too"
        );
    }

    #[test]
    fn restores_a_directory_tree() {
        let tmp = TempDir::new("restore-tree");
        let trash = trash_in(&tmp);
        tmp.create_dir("project/src");
        tmp.create_file("project/src/main.rs", b"code");
        let dir = tmp.join("project");

        let trashed = trash.trash(&dir).expect("trash");
        trash.restore(&trashed.info_path).expect("restore");

        assert_eq!(tmp.read("project/src/main.rs"), b"code");
    }

    /// Restoring must never clobber a file the user currently has -- that
    /// would turn an undo into a deletion.
    #[test]
    fn restore_refuses_to_overwrite_a_file_at_the_original_path() {
        let tmp = TempDir::new("restore-occupied");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("notes.txt", b"old");

        let trashed = trash.trash(&file).expect("trash");
        // The user made a new file with the same name.
        std::fs::write(&file, b"new").expect("recreate");

        match trash.restore(&trashed.info_path) {
            Err(TrashError::Io(_, e)) if e.kind() == io::ErrorKind::AlreadyExists => {}
            other => panic!("expected AlreadyExists, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&file).expect("read"),
            b"new",
            "the current file must be untouched"
        );
        assert!(
            trashed.trashed_path.exists(),
            "the trashed copy must still be recoverable"
        );
    }

    #[test]
    fn restore_recreates_a_missing_parent_directory() {
        let tmp = TempDir::new("restore-parent");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("nested/deep/notes.txt", b"x");

        let trashed = trash.trash(&file).expect("trash");
        // The whole original directory disappeared afterwards.
        std::fs::remove_dir_all(tmp.join("nested")).expect("remove parent");

        let restored = trash.restore(&trashed.info_path).expect("restore");
        assert_eq!(restored, file);
        assert!(file.is_file());
    }

    #[test]
    fn restoring_a_missing_info_file_fails_cleanly() {
        let tmp = TempDir::new("restore-noinfo");
        let trash = trash_in(&tmp);
        trash.dir().ensure_exists().expect("create");

        match trash.restore(&trash.dir().info_dir().join("ghost.trashinfo")) {
            Err(TrashError::Io(..)) => {}
            other => panic!("expected Io, got {other:?}"),
        }
    }

    /// An info record whose `files/` entry vanished must be reported, not
    /// silently "succeed".
    #[test]
    fn restoring_a_record_without_its_file_reports_not_found() {
        let tmp = TempDir::new("restore-nofile");
        let trash = trash_in(&tmp);
        let file = tmp.create_file("notes.txt", b"x");
        let trashed = trash.trash(&file).expect("trash");

        std::fs::remove_file(&trashed.trashed_path).expect("remove from trash");

        match trash.restore(&trashed.info_path) {
            Err(TrashError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_trash_restore_round_trip_preserves_a_symlink() {
        let tmp = TempDir::new("restore-symlink");
        let trash = trash_in(&tmp);
        tmp.create_file("target.txt", b"data");
        let link = tmp.create_symlink("link.txt", "target.txt");

        let trashed = trash.trash(&link).expect("trash");
        trash.restore(&trashed.info_path).expect("restore");

        let meta = std::fs::symlink_metadata(&link).expect("stat");
        assert!(meta.file_type().is_symlink(), "must come back as a symlink");
        assert_eq!(
            std::fs::read_link(&link).expect("readlink"),
            PathBuf::from("target.txt"),
            "the link's target text must be preserved verbatim"
        );
    }

    #[test]
    fn restoring_one_of_two_deduped_files_picks_the_right_one() {
        let tmp = TempDir::new("restore-dedup");
        let trash = trash_in(&tmp);
        let first = tmp.create_file("a/dup.txt", b"first");
        let second = tmp.create_file("b/dup.txt", b"second");

        let one = trash.trash(&first).expect("first");
        let two = trash.trash(&second).expect("second");

        // Restore the *second* (the suffixed one) and check it goes to b/.
        let restored = trash.restore(&two.info_path).expect("restore second");
        assert_eq!(restored, second);
        assert_eq!(tmp.read("b/dup.txt"), b"second");
        assert!(!first.exists(), "the first must still be in the trash");
        assert!(one.trashed_path.exists());
    }
}
