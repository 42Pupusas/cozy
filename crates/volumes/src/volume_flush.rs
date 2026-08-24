//! [`VolumeFlush`] -- push a volume's pending writes to the hardware.
//!
//! # What this is, and what it is not
//!
//! It is **not** an eject, and it must never be labelled as one. Ejecting
//! means unmounting, and `umount(2)` needs `CAP_SYS_ADMIN` in the general
//! case. An unprivileged desktop application cannot do it without a setuid
//! helper or a privileged daemon.
//!
//! What it *can* do is the part that actually protects data. When a file
//! finishes copying to a USB stick, the kernel has usually only recorded it in
//! the page cache. Pulling the stick then loses it. `syncfs(2)` forces those
//! pages to the device, needs no privilege beyond an open descriptor, and
//! removes the window in which unplugging is destructive.
//!
//! So the honest offer to the user is "write everything out now, then it is
//! safe to unplug" -- not "eject", which would promise an unmount that never
//! happened.
//!
//! # Why `syncfs` and not `sync`
//!
//! `sync(2)` flushes *every* filesystem. On a machine with a busy internal
//! disk that can block for a long time for reasons that have nothing to do
//! with the volume the user asked about. `syncfs(2)` flushes only the
//! filesystem holding the given descriptor.

use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

unsafe extern "C" {
    /// Flush the filesystem containing `fd`.
    ///
    /// Declared here rather than pulled from a binding crate because this is
    /// the only libc call the crate makes, and `crates/volumes` otherwise has
    /// no dependencies at all.
    fn syncfs(fd: i32) -> i32;
}

/// Why a flush did not happen.
#[derive(Debug)]
pub enum FlushError {
    /// The mount point could not be opened.
    Unreachable(PathBuf, io::Error),
    /// The kernel refused the flush.
    Failed(PathBuf, io::Error),
}

impl std::fmt::Display for FlushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(path, error) => {
                write!(f, "cannot open {}: {error}", path.display())
            }
            Self::Failed(path, error) => {
                write!(f, "cannot flush {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for FlushError {}

/// Flushes a volume's pending writes.
///
/// A unit struct rather than a bare function so the operation has an owner
/// and a place for a future privileged backend to sit beside it.
pub struct VolumeFlush;

impl VolumeFlush {
    /// The user-facing name of this action.
    ///
    /// Deliberately not "Eject" or "Safely remove": both promise an unmount
    /// that this cannot perform. The wording is the honest part of the
    /// feature, so it lives with the code rather than in the UI layer where
    /// it could drift.
    pub const LABEL: &'static str = "Flush writes";

    /// What the action guarantees, for a tooltip or status line.
    pub const DESCRIPTION: &'static str =
        "Write pending changes to the device. It is then safe to unplug.";

    /// Flush the filesystem containing `path`.
    ///
    /// Opening the directory is enough; no write permission is needed, which
    /// is why this works on a volume the user can only read.
    pub fn flush(path: &Path) -> Result<(), FlushError> {
        let dir = std::fs::File::open(path)
            .map_err(|error| FlushError::Unreachable(path.to_path_buf(), error))?;
        // SAFETY: `dir` owns a valid open descriptor for the whole call, and
        // `syncfs` only reads it. The descriptor is closed by `File::drop`
        // after this returns, never by the callee.
        let result = unsafe { syncfs(dir.as_raw_fd()) };
        if result == 0 {
            return Ok(());
        }
        Err(FlushError::Failed(
            path.to_path_buf(),
            io::Error::last_os_error(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{FlushError, VolumeFlush};
    use crate::test_support::TempDir;
    use std::path::Path;

    /// The real syscall against a real directory. The temp dir is on a
    /// writable filesystem, so this must succeed on any machine.
    #[test]
    fn flushing_a_real_directory_succeeds() {
        let tmp = TempDir::new("flush-ok");
        tmp.write("data.txt", "contents");
        VolumeFlush::flush(tmp.path()).expect("a real directory can be flushed");
    }

    /// The case the feature exists for: data written but not yet on the
    /// device. The flush must return only once the kernel has accepted it.
    #[test]
    fn flushing_after_a_write_reports_success() {
        let tmp = TempDir::new("flush-write");
        for index in 0..32 {
            tmp.write(&format!("file{index}.bin"), &"x".repeat(4096));
        }
        VolumeFlush::flush(tmp.path()).expect("pending writes flush");
    }

    /// A device unplugged between listing it and clicking flush is the
    /// obvious race, and it must be an error rather than a panic.
    #[test]
    fn a_missing_path_is_an_error_not_a_panic() {
        let error = VolumeFlush::flush(Path::new("/nonexistent-volume-for-tests"))
            .expect_err("a missing path cannot be flushed");
        assert!(matches!(error, FlushError::Unreachable(..)));
    }

    /// The message must name the path, since the user may have several
    /// volumes attached.
    #[test]
    fn the_error_message_names_the_path() {
        let error = VolumeFlush::flush(Path::new("/nonexistent-volume-for-tests"))
            .expect_err("must fail");
        assert!(
            error.to_string().contains("/nonexistent-volume-for-tests"),
            "got: {error}"
        );
    }

    /// Read-only volumes are common (optical discs, write-protected cards)
    /// and flushing one is a no-op that must still succeed rather than
    /// reporting a scary failure.
    #[test]
    fn flushing_a_read_only_filesystem_succeeds() {
        VolumeFlush::flush(Path::new("/sys")).expect("a read-only filesystem flushes trivially");
    }

    /// The label must not promise an unmount this cannot perform.
    #[test]
    fn the_action_is_not_labelled_as_an_eject() {
        let label = VolumeFlush::LABEL.to_lowercase();
        assert!(!label.contains("eject"), "must not promise an unmount");
        assert!(!label.contains("remove"), "must not promise an unmount");
        assert!(VolumeFlush::DESCRIPTION.contains("unplug"));
    }
}
