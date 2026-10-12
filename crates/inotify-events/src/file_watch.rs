//! [`FileWatch`] -- reports saves of one file, however the editor saves it.
//!
//! The watch is on the file's directory, filtered by name, because an editor
//! that renames a new file into place replaces the inode a watch on the file
//! itself would follow.

use std::ffi::CString;
use std::path::Path;

use crate::InotifyEvents;

pub struct FileWatch {
    ino: ququmatz::Inotify,
    name: String,
}

impl FileWatch {
    /// Watch `path`, or `None` when its directory does not exist or inotify
    /// is unavailable.
    #[must_use]
    pub fn on(path: &Path) -> Option<Self> {
        let dir = CString::new(path.parent()?.as_os_str().as_encoded_bytes()).ok()?;
        let name = path.file_name()?.to_string_lossy().into_owned();
        let ino = ququmatz::Inotify::new().ok()?;
        ino.add_watch(
            &dir,
            ququmatz::WatchMask::CLOSE_WRITE | ququmatz::WatchMask::MOVED_TO,
        )
        .ok()?;
        Some(Self { ino, name })
    }

    #[must_use]
    pub const fn raw_fd(&self) -> i32 {
        use ququmatz::types::RawFd;
        RawFd::as_i32(self.ino.fd())
    }

    /// Whether the file was saved since the last call. Drains the queue.
    #[must_use]
    pub fn touched(&self) -> bool {
        InotifyEvents::any_touched(&self.ino, &self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::FileWatch;

    #[test]
    fn a_save_or_a_rename_into_place_is_reported_once() {
        let dir = std::env::temp_dir().join(format!("file_watch_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let watch = FileWatch::on(&path).expect("watch");
        assert!(!watch.touched());

        std::fs::write(&path, "a").unwrap();
        assert!(watch.touched());
        assert!(!watch.touched());

        std::fs::write(dir.join("config.toml.tmp"), "b").unwrap();
        assert!(!watch.touched(), "another file is not this one");
        std::fs::rename(dir.join("config.toml.tmp"), &path).unwrap();
        assert!(watch.touched());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_directory_cannot_be_watched() {
        let path = std::env::temp_dir().join("file_watch_no_such_dir/config.toml");
        assert!(FileWatch::on(&path).is_none());
    }
}
