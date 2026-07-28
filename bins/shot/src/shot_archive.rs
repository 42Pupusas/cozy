//! Where a kept screenshot goes, and what it is called.
//!
//! Split out from `shot_options` and `shot_writer` because this is *policy*,
//! not parsing or I/O: which directory an archived capture lands in, how its
//! name encodes the moment it was taken, and what happens when that name is
//! already taken. All of it is decided from values passed in -- a root
//! directory and a `Timestamp` -- so the rules are testable with fixed inputs
//! instead of racing the wall clock or depending on the user's `$HOME`.

use localtime::Timestamp;
use std::path::{Path, PathBuf};

/// Directory, relative to the user's home, that archived captures go in.
///
/// `Images/Screenshots` rather than the XDG pictures directory: this machine's
/// `user-dirs.dirs` points `XDG_PICTURES_DIR` at `$HOME` itself, so honouring
/// it would scatter screenshots across the home directory.
const DEFAULT_SUBDIR: &str = "Images/Screenshots";

/// Environment variable that overrides the archive directory outright.
const DIR_OVERRIDE: &str = "SHOT_DIR";

/// Resolves archive destinations for kept screenshots.
pub struct ShotArchive {
    root: PathBuf,
}

impl ShotArchive {
    /// An archive rooted at `root`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The archive the user's environment asks for.
    ///
    /// `$SHOT_DIR` wins so a script can redirect captures without touching
    /// the keybind; otherwise `$HOME/Images/Screenshots`.
    ///
    /// # Errors
    ///
    /// Fails only when neither `$SHOT_DIR` nor `$HOME` is set, which means
    /// there is no way to guess where the user wants files kept.
    pub fn from_environment() -> Result<Self, String> {
        if let Some(dir) = std::env::var_os(DIR_OVERRIDE).filter(|d| !d.is_empty()) {
            return Ok(Self::new(PathBuf::from(dir)));
        }
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| Self::new(Path::new(&home).join(DEFAULT_SUBDIR)))
            .ok_or_else(|| {
                format!("cannot decide where to save: neither {DIR_OVERRIDE} nor HOME is set")
            })
    }

    /// The directory captures are written into.
    ///
    /// Only the tests read this today -- the production path goes straight
    /// through `reserve` -- but it is the natural accessor for the type and
    /// keeping it means the fixtures can clean up after themselves.
    #[cfg(test)]
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The bare file name for a capture taken at `at`.
    ///
    /// Colons are deliberately avoided in the time portion even though local
    /// filesystems allow them: they are the separator in `$PATH`-style lists
    /// and are rejected outright by FAT/exFAT, which is exactly where a
    /// screenshot gets copied when someone moves it to a USB stick.
    #[must_use]
    pub fn file_name(at: &Timestamp) -> String {
        format!(
            "Screenshot_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.png",
            at.year, at.month, at.day, at.hour, at.minute, at.second
        )
    }

    /// A free path for a capture taken at `at`, creating the directory.
    ///
    /// Two captures inside the same second are rare but entirely possible on a
    /// held-down key, so a taken name gets a `-1`, `-2`, ... suffix rather
    /// than silently overwriting the earlier image.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be created, naming it -- a screenshot
    /// vanishing because its folder was unwritable should say so.
    pub fn reserve(&self, at: &Timestamp) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.root)
            .map_err(|e| format!("creating {}: {e}", self.root.display()))?;
        Ok(self.free_path(&Self::file_name(at)))
    }

    /// First unused path for `name` within the archive.
    ///
    /// Bounded rather than looping forever: if a thousand captures share one
    /// second something is badly wrong, and returning the last candidate lets
    /// the write fail with a real I/O error instead of hanging the keybind.
    fn free_path(&self, name: &str) -> PathBuf {
        let candidate = self.root.join(name);
        if !candidate.exists() {
            return candidate;
        }
        // Suffix before the extension so the file stays a recognisable `.png`
        // to every image viewer.
        let stem = name.strip_suffix(".png").unwrap_or(name);
        for index in 1..1000 {
            let candidate = self.root.join(format!("{stem}-{index}.png"));
            if !candidate.exists() {
                return candidate;
            }
        }
        self.root.join(format!("{stem}-999.png"))
    }
}

#[cfg(test)]
mod tests {
    use super::ShotArchive;
    use localtime::Timestamp;

    /// A fixed instant, so no assertion here races the clock.
    const SAMPLE: Timestamp = Timestamp {
        year: 2026,
        month: 7,
        day: 28,
        hour: 9,
        minute: 5,
        second: 3,
    };

    /// A scratch archive under a unique temp directory.
    struct ArchiveFixture {
        archive: ShotArchive,
    }

    impl ArchiveFixture {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("shot-archive-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            Self {
                archive: ShotArchive::new(root),
            }
        }
    }

    impl Drop for ArchiveFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.archive.root());
        }
    }

    #[test]
    fn the_name_encodes_the_moment_zero_padded() {
        assert_eq!(
            ShotArchive::file_name(&SAMPLE),
            "Screenshot_2026-07-28_09-05-03.png"
        );
    }

    #[test]
    fn the_name_avoids_colons_so_it_survives_a_fat_usb_stick() {
        let name = ShotArchive::file_name(&SAMPLE);
        assert!(!name.contains(':'), "colon in {name}");
    }

    #[test]
    fn names_sort_chronologically_as_plain_text() {
        // The whole point of a leading zero-padded year-first name: `ls` and
        // any file manager sort by time without understanding the format.
        let earlier = ShotArchive::file_name(&SAMPLE);
        let later = ShotArchive::file_name(&Timestamp { hour: 10, ..SAMPLE });
        assert!(earlier < later, "{earlier} should sort before {later}");
    }

    #[test]
    fn reserving_creates_the_directory_that_did_not_exist() {
        let fixture = ArchiveFixture::new("creates");
        assert!(!fixture.archive.root().exists());
        let path = fixture.archive.reserve(&SAMPLE).expect("reserves");
        assert!(fixture.archive.root().is_dir());
        assert_eq!(path.parent(), Some(fixture.archive.root()));
    }

    #[test]
    fn a_first_capture_uses_the_plain_name() {
        let fixture = ArchiveFixture::new("plain");
        let path = fixture.archive.reserve(&SAMPLE).expect("reserves");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("Screenshot_2026-07-28_09-05-03.png")
        );
    }

    #[test]
    fn a_second_capture_in_the_same_second_does_not_overwrite_the_first() {
        let fixture = ArchiveFixture::new("collide");
        let first = fixture.archive.reserve(&SAMPLE).expect("reserves");
        std::fs::write(&first, b"first").expect("writes");
        let second = fixture.archive.reserve(&SAMPLE).expect("reserves");
        assert_ne!(first, second);
        assert_eq!(
            second.file_name().and_then(|n| n.to_str()),
            Some("Screenshot_2026-07-28_09-05-03-1.png")
        );
        // The original must still be intact -- that is the actual guarantee.
        assert_eq!(std::fs::read(&first).expect("still there"), b"first");
    }

    #[test]
    fn a_suffixed_name_keeps_the_png_extension() {
        let fixture = ArchiveFixture::new("extension");
        let first = fixture.archive.reserve(&SAMPLE).expect("reserves");
        std::fs::write(&first, b"x").expect("writes");
        let second = fixture.archive.reserve(&SAMPLE).expect("reserves");
        assert_eq!(second.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn collisions_keep_producing_distinct_names() {
        let fixture = ArchiveFixture::new("many");
        let mut seen = Vec::new();
        for _ in 0..5 {
            let path = fixture.archive.reserve(&SAMPLE).expect("reserves");
            assert!(!seen.contains(&path), "repeated path {}", path.display());
            std::fs::write(&path, b"x").expect("writes");
            seen.push(path);
        }
    }
}
