//! One audited `statvfs(2)` FFI call, shared by anything that needs raw
//! filesystem capacity figures -- today that's `sysmon`'s disk-usage
//! percentage and `bins/files`'s sidebar storage footer.
//!
//! Both call sites used to declare their own `#[repr(C)] struct Statvfs`,
//! their own `mem::zeroed()`, and their own `unsafe extern "C" { fn
//! statvfs(...) }` -- identical layouts, identical "an all-zero struct is
//! a valid bit pattern" justification, just copy-pasted. That's exactly
//! the shape `crates/localtime` was already pulled out of `statusbar` for
//! (see that crate's doc comment): duplicated raw FFI must have a single
//! owner, so there is one audited `unsafe` block instead of two that can
//! silently drift apart (e.g. one call site missing a struct field the
//! kernel still expects to fill).

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The full `struct statvfs` layout (Linux, LP64). Declared with every
/// field because the kernel writes the whole struct: a short definition
/// would have `statvfs` scribble past the end of our allocation. Field
/// names mirror the C ones (`man 3 statvfs`) so the layout can be checked
/// against the man page directly.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawStatvfs {
    bsize: u64,
    frsize: u64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    favail: u64,
    fsid: u64,
    flag: u64,
    namemax: u64,
    /// Padding the kernel reserves; present so our allocation is the size
    /// the kernel expects to fill.
    spare: [u32; 6],
}

impl Default for RawStatvfs {
    fn default() -> Self {
        // SAFETY: an all-zero `RawStatvfs` is a valid bit pattern -- every
        // field is a plain unsigned integer or an array of them.
        unsafe { std::mem::zeroed() }
    }
}

unsafe extern "C" {
    fn statvfs(path: *const i8, buf: *mut RawStatvfs) -> i32;
}

/// The subset of `statvfs(2)`'s answer callers actually need: block size,
/// block counts, and the fragment-size fallback rule for converting them
/// to bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStat {
    frsize: u64,
    bsize: u64,
    blocks: u64,
    bavail: u64,
}

impl DiskStat {
    /// Read `path`'s filesystem stats. `None` on any failure: an
    /// unreadable path, a path with an interior NUL (which cannot exist on
    /// a Unix filesystem, so this is belt and braces), or `statvfs`
    /// itself failing.
    #[must_use]
    pub fn for_path(path: &Path) -> Option<Self> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut raw = RawStatvfs::default();
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives
        // the call, and `raw` is a live, correctly sized, writable
        // `RawStatvfs`.
        let rc = unsafe { statvfs(c_path.as_ptr(), &raw mut raw) };
        if rc != 0 {
            return None;
        }
        Some(Self {
            frsize: raw.frsize,
            bsize: raw.bsize,
            blocks: raw.blocks,
            bavail: raw.bavail,
        })
    }

    /// The byte size of one block, per `statvfs`'s own documented
    /// fallback: `frsize` (fragment size) when reported, otherwise
    /// `bsize` -- some filesystems report `frsize` as zero.
    const fn unit_bytes(self) -> u64 {
        if self.frsize > 0 { self.frsize } else { self.bsize }
    }

    /// Total filesystem capacity in bytes. `None` only on a `u64`
    /// overflow, which in practice means a filesystem far larger than any
    /// that exists today.
    #[must_use]
    pub const fn total_bytes(self) -> Option<u64> {
        self.blocks.checked_mul(self.unit_bytes())
    }

    /// Bytes available to an unprivileged process. Saturates rather than
    /// overflows -- `bavail * unit` cannot practically exceed `u64`, but a
    /// saturating fallback is cheaper to reason about than an `Option`
    /// here has any right to be.
    #[must_use]
    pub const fn available_bytes(self) -> u64 {
        self.bavail.saturating_mul(self.unit_bytes())
    }

    /// Percentage full, 0-100, computed directly from block counts (a
    /// unit-independent ratio, so no byte conversion -- and so no
    /// overflow risk -- is needed). `0.0` for a zero-block pseudo-
    /// filesystem (e.g. `/proc`), which has no meaningful capacity to
    /// report.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn percent_used(self) -> f32 {
        if self.blocks == 0 {
            return 0.0;
        }
        100.0 * (1.0 - self.bavail as f32 / self.blocks as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_reports_a_plausible_nonzero_capacity() {
        // Can't pin an exact figure (depends on the box this runs on), but
        // `/` should always be a real, non-degenerate filesystem.
        let stat = DiskStat::for_path(Path::new("/")).expect("/ must be statvfs-able");
        assert!(stat.total_bytes().unwrap_or(0) > 0);
        assert!((0.0..=100.0).contains(&stat.percent_used()));
    }

    #[test]
    fn a_missing_path_reports_none() {
        assert!(DiskStat::for_path(Path::new("/definitely/does/not/exist/anywhere")).is_none());
    }

    #[test]
    fn a_path_with_an_interior_nul_reports_none_rather_than_panicking() {
        let bad = std::ffi::OsStr::new("/tmp/bad\0name");
        assert!(DiskStat::for_path(Path::new(bad)).is_none());
    }

    #[test]
    fn zero_blocks_reports_zero_percent_rather_than_dividing_by_zero() {
        let stat = DiskStat {
            frsize: 4096,
            bsize: 4096,
            blocks: 0,
            bavail: 0,
        };
        // Exact comparison is deliberate: `percent_used`'s zero-block
        // path is an early `return 0.0`, not a computed float, so there
        // is no rounding to tolerate.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(stat.percent_used(), 0.0);
        }
    }

    #[test]
    fn frsize_zero_falls_back_to_bsize_for_byte_totals() {
        let stat = DiskStat {
            frsize: 0,
            bsize: 512,
            blocks: 1000,
            bavail: 400,
        };
        assert_eq!(stat.total_bytes(), Some(512_000));
        assert_eq!(stat.available_bytes(), 204_800);
    }

    #[test]
    fn a_half_full_disk_reads_fifty_percent() {
        let stat = DiskStat {
            frsize: 1,
            bsize: 1,
            blocks: 1000,
            bavail: 500,
        };
        assert!((stat.percent_used() - 50.0).abs() < 0.01);
    }
}
