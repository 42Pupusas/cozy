//! [`Storage`] -- the sidebar's disk-usage footer: which filesystem the open
//! directory lives on, how full it is, and the meter that shows it.
//!
//! The guide pins this to the bottom of the sidebar as ambient context: you
//! see how much room is left without asking. That framing drives two design
//! choices here.
//!
//! First, the *numbers* and the *syscall* are separated. [`Usage`] is plain
//! arithmetic over block counts, so every rounding rule and every degenerate
//! case (a zero-block pseudo-filesystem, a full disk, a disk reporting more
//! free blocks than it has) is testable without needing a real filesystem in
//! a particular state -- which is not something a test can arrange.
//!
//! Second, a failed `statvfs` is not an error to report. Some paths simply
//! have no meaningful figure (a `/proc` entry), and the honest response is to
//! draw nothing rather than to push a scary message into a UI whose job is to
//! be ambient. So the fallible boundary returns `Option`.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The subset of `struct statvfs` this module reads.
///
/// Declared with the full field list because the kernel writes the whole
/// struct: a short definition would have `statvfs` scribble past the end of
/// our allocation. Field names mirror the C ones so they can be checked
/// against `man 3 statvfs` directly.
#[repr(C)]
#[derive(Clone, Copy)]
struct Statvfs {
    /// Filesystem block size.
    bsize: u64,
    /// Fragment size.
    frsize: u64,
    /// Size of the filesystem in `frsize` units.
    blocks: u64,
    /// Free blocks.
    bfree: u64,
    /// Free blocks available to an unprivileged process.
    bavail: u64,
    /// Total inodes.
    files: u64,
    /// Free inodes.
    ffree: u64,
    /// Free inodes available to an unprivileged process.
    favail: u64,
    /// Filesystem id.
    fsid: u64,
    /// Mount flags.
    flag: u64,
    /// Maximum filename length.
    namemax: u64,
    /// Padding the kernel reserves; present so our allocation is the size
    /// the kernel expects to fill.
    spare: [u32; 6],
}

impl Default for Statvfs {
    fn default() -> Self {
        // SAFETY: an all-zero `Statvfs` is a valid bit pattern -- every field
        // is a plain unsigned integer or an array of them.
        unsafe { std::mem::zeroed() }
    }
}

unsafe extern "C" {
    fn statvfs(path: *const i8, buf: *mut Statvfs) -> i32;
}

/// How full a filesystem is, in bytes.
///
/// Constructed from raw totals so the formatting and percentage rules can be
/// tested against any disk state, including ones that cannot be arranged on a
/// real machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Total capacity in bytes.
    total: u64,
    /// Bytes in use.
    used: u64,
}

impl Usage {
    /// Build a usage figure from a total and an available count.
    ///
    /// `available` is clamped to `total`: a filesystem may legitimately
    /// report more *free* blocks than an unprivileged process can use, and
    /// reserved-space accounting can otherwise produce a negative "used"
    /// that would wrap into an absurd number.
    #[must_use]
    pub const fn new(total: u64, available: u64) -> Self {
        let available = if available > total { total } else { available };
        Self {
            total,
            used: total - available,
        }
    }

    /// Total capacity in bytes.
    ///
    /// Only the tests read this today -- the footer shows derived strings --
    /// but the raw figure is what makes those derivations checkable, and a
    /// type that computes percentages while hiding its inputs is harder to
    /// trust than one that exposes both.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn total(self) -> u64 {
        self.total
    }

    /// Bytes in use.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn used(self) -> u64 {
        self.used
    }

    /// Whether there is any capacity to report.
    ///
    /// Pseudo-filesystems like `/proc` report zero blocks, and a meter of a
    /// zero-byte disk is meaningless -- the caller draws nothing instead.
    #[must_use]
    pub const fn is_meaningful(self) -> bool {
        self.total > 0
    }

    /// Percentage used, 0-100, rounded to nearest.
    ///
    /// Computed in `u128` because `used * 100` overflows `u64` past roughly
    /// 184 petabytes -- unusual today but not absurd for an array, and a
    /// silent wrap would report a full disk as empty.
    #[must_use]
    pub fn percent(self) -> u32 {
        if self.total == 0 {
            return 0;
        }
        let total = u128::from(self.total);
        // The `+ total / 2` is round-to-nearest done in integers.
        let pct = (u128::from(self.used) * 100 + total / 2) / total;
        // `used <= total` by construction, so this cannot exceed 100; the
        // fallback is a belt-and-braces clamp rather than a live branch.
        u32::try_from(pct).unwrap_or(100).min(100)
    }

    /// The fraction of the meter to fill, as a width in `full` pixels.
    ///
    /// Derived from the byte counts rather than from [`Self::percent`] so a
    /// wide meter is not quantised to 100 steps.
    #[must_use]
    pub fn meter_width(self, full: i32) -> i32 {
        if self.total == 0 || full <= 0 {
            return 0;
        }
        let Ok(track) = u128::try_from(full) else {
            return 0;
        };
        let filled = u128::from(self.used) * track / u128::from(self.total);
        // `filled <= full` because `used <= total`, so the conversion cannot
        // fail; clamping to `full` rather than 0 keeps a hypothetical failure
        // from reading as an empty disk.
        i32::try_from(filled).unwrap_or(full).min(full)
    }

    /// The percentage as the guide renders it: a bare number and a sign.
    #[must_use]
    pub fn percent_label(self) -> String {
        format!("{}%", self.percent())
    }

    /// Capacity in the guide's shorthand -- `128g`, `1t`, `512m`.
    ///
    /// Deliberately coarser than the size column's format: this is ambient
    /// context in a 10px caption, where `1.4t` earns its extra glyphs but
    /// `1.44t` does not.
    #[must_use]
    pub fn capacity_label(self) -> String {
        Self::short_bytes(self.total)
    }

    /// The footer's right-hand readout: how full, and how big.
    ///
    /// Both halves in one string because they are one thought -- "61% of
    /// 128g" answers "do I have room?", which neither number does alone.
    #[must_use]
    pub fn summary(self) -> String {
        format!("{} of {}", self.percent_label(), self.capacity_label())
    }

    /// Format a byte count in the footer's shorthand.
    ///
    /// Lowercase single-letter units, one decimal only below ten, matching
    /// the guide's `128g` / `1t` captions.
    ///
    /// Done in integer arithmetic rather than with an `f64` divide: a
    /// multi-terabyte array exceeds `f64`'s 52-bit mantissa, and while the
    /// visible error would be small, there is no reason to accept rounding
    /// noise in a calculation this simple.
    fn short_bytes(bytes: u64) -> String {
        const UNITS: [&str; 5] = ["b", "k", "m", "g", "t"];
        let mut divisor: u128 = 1;
        let mut unit = 0;
        while u128::from(bytes) / divisor >= 1024 && unit + 1 < UNITS.len() {
            divisor *= 1024;
            unit += 1;
        }
        if unit == 0 {
            return format!("{bytes}{}", UNITS[0]);
        }
        // Value in tenths of the chosen unit, so one decimal place is
        // available without ever building a float.
        let tenths = u128::from(bytes) * 10 / divisor;
        if tenths < 100 {
            let whole = tenths / 10;
            let frac = tenths % 10;
            // Trim a trailing `.0` so `1.0t` reads as `1t`, as in the guide.
            if frac == 0 {
                format!("{whole}{}", UNITS[unit])
            } else {
                format!("{whole}.{frac}{}", UNITS[unit])
            }
        } else {
            // Round to nearest whole unit.
            format!("{}{}", (tenths + 5) / 10, UNITS[unit])
        }
    }
}

/// The filesystem backing a path: its device name and how full it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    /// The label shown at the footer's left -- a device path when we can name
    /// one, otherwise the mount point.
    device: String,
    /// The usage figures behind the meter.
    usage: Usage,
}

impl Storage {
    /// Read the filesystem containing `path`.
    ///
    /// `None` when the path has no meaningful figures -- an unreadable path,
    /// or a pseudo-filesystem reporting zero blocks. The footer is ambient,
    /// so having nothing to say is a normal outcome rather than an error.
    #[must_use]
    pub fn of(path: &Path) -> Option<Self> {
        let usage = Self::usage_of(path)?;
        if !usage.is_meaningful() {
            return None;
        }
        Some(Self {
            device: Self::device_for(path),
            usage,
        })
    }

    /// Read raw usage for `path` via `statvfs`.
    fn usage_of(path: &Path) -> Option<Usage> {
        // A path containing an interior NUL cannot be passed to a C API; it
        // also cannot exist on a Unix filesystem, so this is belt and braces.
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut raw = Statvfs::default();
        // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
        // call, and `raw` is a live, correctly sized, writable `Statvfs`.
        let rc = unsafe { statvfs(c_path.as_ptr(), &raw mut raw) };
        if rc != 0 {
            return None;
        }
        // `frsize` is the unit `blocks` and `bavail` are counted in; it can
        // be reported as zero by odd filesystems, in which case `bsize` is
        // the documented fallback.
        let unit = if raw.frsize > 0 {
            raw.frsize
        } else {
            raw.bsize
        };
        let total = raw.blocks.checked_mul(unit)?;
        let available = raw.bavail.saturating_mul(unit);
        Some(Usage::new(total, available))
    }

    /// The name to show for the filesystem holding `path`.
    ///
    /// `statvfs` does not report a device name, and parsing `/proc/mounts` to
    /// find one would mean matching mount points by prefix length and getting
    /// bind mounds and overlays wrong in ways that are hard to notice. The
    /// mount point is both honest and more useful to a person than a
    /// `/dev/nvme0n1p2` they would have to look up anyway.
    fn device_for(path: &Path) -> String {
        Self::mount_point(path).unwrap_or_else(|| "/".to_owned())
    }

    /// Walk up from `path` to the directory where the device number changes:
    /// that boundary is the mount point.
    ///
    /// Uses device identity rather than string matching, so bind mounts and
    /// nested filesystems land on the right answer.
    fn mount_point(path: &Path) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let start = std::fs::metadata(path).ok()?;
        let device = start.dev();
        let mut best = path;
        let mut cursor = path;
        while let Some(parent) = cursor.parent() {
            match std::fs::metadata(parent) {
                // Still the same filesystem: the mount point is at least
                // this far up.
                Ok(meta) if meta.dev() == device => {
                    best = parent;
                    cursor = parent;
                }
                // The device changed, or the parent is unreadable: whatever
                // we last confirmed is the boundary.
                _ => break,
            }
        }
        Some(best.to_string_lossy().into_owned())
    }

    /// The device or mount label for the footer's left side.
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The usage figures for the meter and the percentage.
    #[must_use]
    pub const fn usage(&self) -> Usage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::{Storage, Usage};

    #[test]
    fn a_half_full_disk_reads_fifty_percent() {
        let usage = Usage::new(1000, 500);
        assert_eq!(usage.percent(), 50);
        assert_eq!(usage.used(), 500);
        assert_eq!(usage.total(), 1000);
    }

    #[test]
    fn an_empty_disk_reads_zero_and_a_full_one_reads_a_hundred() {
        assert_eq!(Usage::new(1000, 1000).percent(), 0);
        assert_eq!(Usage::new(1000, 0).percent(), 100);
    }

    /// A pseudo-filesystem reports no blocks at all; a meter over it would be
    /// meaningless, so the caller is told there is nothing to draw.
    #[test]
    fn a_zero_capacity_filesystem_is_not_meaningful() {
        let usage = Usage::new(0, 0);
        assert!(!usage.is_meaningful());
        assert_eq!(usage.percent(), 0);
        assert_eq!(usage.meter_width(100), 0);
    }

    /// Reserved-space accounting can report more available than total; that
    /// must clamp rather than wrap into an enormous "used".
    #[test]
    fn more_available_than_total_clamps_instead_of_wrapping() {
        let usage = Usage::new(1000, 5000);
        assert_eq!(usage.used(), 0);
        assert_eq!(usage.percent(), 0);
    }

    /// `used * 100` overflows u64 past ~184 petabytes, which would silently
    /// report a full array as empty.
    #[test]
    fn a_petabyte_array_does_not_overflow_its_percentage() {
        let petabyte = 1024_u64.pow(5);
        let usage = Usage::new(petabyte * 8, petabyte * 2);
        assert_eq!(usage.percent(), 75);
    }

    #[test]
    fn the_percentage_rounds_to_nearest() {
        // 0.5% up, and just under a half stays down.
        assert_eq!(Usage::new(1000, 995).percent(), 1);
        assert_eq!(Usage::new(1000, 996).percent(), 0);
    }

    #[test]
    fn the_meter_fills_in_proportion_to_use() {
        let usage = Usage::new(1000, 390);
        assert_eq!(usage.meter_width(100), 61);
    }

    /// The meter is derived from bytes, not from the rounded percentage, so a
    /// wide meter is not quantised to 100 steps.
    #[test]
    fn the_meter_is_finer_than_the_percentage() {
        let a = Usage::new(100_000, 49_000).meter_width(1000);
        let b = Usage::new(100_000, 48_900).meter_width(1000);
        assert_ne!(a, b);
    }

    #[test]
    fn the_meter_never_exceeds_its_track_or_goes_negative() {
        for available in [0, 1, 500, 999, 1000, 5000] {
            let usage = Usage::new(1000, available);
            let width = usage.meter_width(200);
            assert!((0..=200).contains(&width), "{available} gave {width}");
        }
        assert_eq!(Usage::new(1000, 500).meter_width(0), 0);
        assert_eq!(Usage::new(1000, 500).meter_width(-5), 0);
    }

    #[test]
    fn capacities_use_the_guides_shorthand() {
        assert_eq!(
            Usage::new(128 * 1024_u64.pow(3), 0).capacity_label(),
            "128g"
        );
        assert_eq!(Usage::new(1024_u64.pow(4), 0).capacity_label(), "1t");
        assert_eq!(Usage::new(512 * 1024 * 1024, 0).capacity_label(), "512m");
    }

    /// Below ten the extra digit carries real information; at and above it,
    /// it is noise in a 10px caption.
    #[test]
    fn only_small_values_keep_a_decimal() {
        let g = 1024_u64.pow(3);
        assert_eq!(Usage::new(3 * g + g / 2, 0).capacity_label(), "3.5g");
        assert_eq!(Usage::new(128 * g, 0).capacity_label(), "128g");
    }

    #[test]
    fn plain_bytes_keep_their_unit() {
        assert_eq!(Usage::new(0, 0).capacity_label(), "0b");
        assert_eq!(Usage::new(512, 0).capacity_label(), "512b");
    }

    /// An absurd capacity saturates at the largest unit rather than running
    /// off the end of the table.
    #[test]
    fn an_enormous_capacity_saturates_at_terabytes() {
        let label = Usage::new(u64::MAX, 0).capacity_label();
        assert!(label.ends_with('t'), "got {label}");
    }

    #[test]
    fn the_percent_label_carries_its_sign() {
        assert_eq!(Usage::new(1000, 390).percent_label(), "61%");
    }

    /// The real syscall against a path that certainly exists. Asserts only
    /// what must hold on any machine -- a temp dir is on a real filesystem
    /// with a real capacity.
    #[test]
    fn a_real_directory_reports_a_plausible_filesystem() {
        let dir = std::env::temp_dir();
        let storage = Storage::of(&dir).expect("temp dir is on a real filesystem");
        assert!(storage.usage().is_meaningful());
        assert!(storage.usage().total() > 0);
        assert!(storage.usage().used() <= storage.usage().total());
        assert!((0..=100).contains(&storage.usage().percent()));
        assert!(
            storage.device().starts_with('/'),
            "device {} is not a path",
            storage.device()
        );
    }

    /// A path that does not exist has no figures to report, and that is a
    /// normal outcome rather than an error worth surfacing.
    #[test]
    fn a_missing_path_reports_nothing() {
        let path = std::path::Path::new("/nonexistent-for-files-bin-storage-test");
        assert!(Storage::of(path).is_none());
    }

    /// The mount point is found by device identity, so a subdirectory of a
    /// filesystem resolves to the same label as its root.
    #[test]
    fn a_subdirectory_resolves_to_its_mount_point() {
        let dir = std::env::temp_dir();
        let nested = dir.join("files-bin-storage-probe");
        std::fs::create_dir_all(&nested).expect("can create a temp subdirectory");
        let outer = Storage::of(&dir).expect("temp dir has a filesystem");
        let inner = Storage::of(&nested).expect("subdirectory has a filesystem");
        assert_eq!(outer.device(), inner.device());
        let _ = std::fs::remove_dir(&nested);
    }
}
