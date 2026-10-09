//! [`Storage`] -- the card's disk-usage figures: how full the filesystem the
//! open directory lives on is, and the numbers that say so.
//!
//! The card shows this as ambient context: you see how much room is left
//! without asking. That framing drives two design choices here.
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

use std::path::Path;

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

    /// Total capacity in bytes, exposed so the derived strings are checkable.
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

    /// How much of `full` pixels the used share covers.
    ///
    /// Computed in `u128` because `used * full` can overflow `u64` for an
    /// array of hundreds of petabytes, and a silent wrap would report a full
    /// disk as empty. Derived from the byte counts, so a tall fill is not
    /// quantised to a hundred steps.
    #[must_use]
    pub fn fill_extent(self, full: i32) -> i32 {
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

    /// Capacity in the footer's shorthand -- `128 GiB`, `1 TiB`, `512 MiB`.
    ///
    /// Deliberately coarser than the size column's format: this is ambient
    /// context in a small caption, where `1.4 TiB` earns its extra glyphs but
    /// `1.44 TiB` does not. The figure is a power of 1024, so the unit is
    /// `GiB`, not a bare `g`.
    #[must_use]
    pub fn capacity_label(self) -> String {
        Self::short_bytes(self.total)
    }

    /// Bytes in use, in the same shorthand as [`Self::capacity_label`].
    #[must_use]
    pub fn used_label(self) -> String {
        Self::short_bytes(self.used)
    }

    /// The footer's one readout: `used / total`, e.g. `61.4 GiB / 128 GiB`.
    #[must_use]
    pub fn figures(self) -> String {
        format!("{} / {}", self.used_label(), self.capacity_label())
    }

    /// Format a byte count in the footer's shorthand.
    ///
    /// IEC binary units, one decimal only below ten.
    ///
    /// Done in integer arithmetic rather than with an `f64` divide: a
    /// multi-terabyte array exceeds `f64`'s 52-bit mantissa, and while the
    /// visible error would be small, there is no reason to accept rounding
    /// noise in a calculation this simple.
    fn short_bytes(bytes: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        let mut divisor: u128 = 1;
        let mut unit = 0;
        while u128::from(bytes) / divisor >= 1024 && unit + 1 < UNITS.len() {
            divisor *= 1024;
            unit += 1;
        }
        if unit == 0 {
            return format!("{bytes} {}", UNITS[0]);
        }
        // Value in tenths of the chosen unit, so one decimal place is
        // available without ever building a float.
        let tenths = u128::from(bytes) * 10 / divisor;
        if tenths < 100 {
            let whole = tenths / 10;
            let frac = tenths % 10;
            // Trim a trailing `.0` so `1.0 TiB` reads as `1 TiB`.
            if frac == 0 {
                format!("{whole} {}", UNITS[unit])
            } else {
                format!("{whole}.{frac} {}", UNITS[unit])
            }
        } else {
            // Round to nearest whole unit.
            format!("{} {}", (tenths + 5) / 10, UNITS[unit])
        }
    }
}

/// The filesystem backing a path and how full it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
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
        Some(Self { usage })
    }

    /// Read raw usage for `path` via [`diskstat::DiskStat`].
    fn usage_of(path: &Path) -> Option<Usage> {
        let stat = diskstat::DiskStat::for_path(path)?;
        let total = stat.total_bytes()?;
        Some(Usage::new(total, stat.available_bytes()))
    }

    /// The usage figures behind the fill and the footer.
    #[must_use]
    pub const fn usage(&self) -> Usage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::{Storage, Usage};

    #[test]
    fn a_half_full_disk_fills_half_the_extent() {
        let usage = Usage::new(1000, 500);
        assert_eq!(usage.fill_extent(200), 100);
        assert_eq!(usage.used(), 500);
        assert_eq!(usage.total(), 1000);
    }

    #[test]
    fn an_empty_disk_fills_nothing_and_a_full_one_fills_everything() {
        assert_eq!(Usage::new(1000, 1000).fill_extent(200), 0);
        assert_eq!(Usage::new(1000, 0).fill_extent(200), 200);
    }

    /// A pseudo-filesystem reports no blocks at all; a fill over it would be
    /// meaningless, so the caller is told there is nothing to draw.
    #[test]
    fn a_zero_capacity_filesystem_is_not_meaningful() {
        let usage = Usage::new(0, 0);
        assert!(!usage.is_meaningful());
        assert_eq!(usage.fill_extent(100), 0);
    }

    /// Reserved-space accounting can report more available than total; that
    /// must clamp rather than wrap into an enormous "used".
    #[test]
    fn more_available_than_total_clamps_instead_of_wrapping() {
        let usage = Usage::new(1000, 5000);
        assert_eq!(usage.used(), 0);
        assert_eq!(usage.fill_extent(100), 0);
    }

    /// `used * extent` can overflow u64 for an array of hundreds of
    /// petabytes, which would silently report it as empty.
    #[test]
    fn an_enormous_array_does_not_overflow_its_fill() {
        let petabyte = 1024_u64.pow(5);
        let usage = Usage::new(petabyte * 8192, petabyte * 2048);
        assert_eq!(usage.fill_extent(1000), 750);
    }

    #[test]
    fn the_fill_is_proportional_to_use() {
        let usage = Usage::new(1000, 390);
        assert_eq!(usage.fill_extent(100), 61);
    }

    #[test]
    fn the_fill_is_finer_than_a_hundred_steps() {
        let a = Usage::new(100_000, 49_000).fill_extent(1000);
        let b = Usage::new(100_000, 48_900).fill_extent(1000);
        assert_ne!(a, b);
    }

    #[test]
    fn the_fill_never_exceeds_its_extent_or_goes_negative() {
        for available in [0, 1, 500, 999, 1000, 5000] {
            let usage = Usage::new(1000, available);
            let extent = usage.fill_extent(200);
            assert!((0..=200).contains(&extent), "{available} gave {extent}");
        }
        assert_eq!(Usage::new(1000, 500).fill_extent(0), 0);
        assert_eq!(Usage::new(1000, 500).fill_extent(-5), 0);
    }

    #[test]
    fn capacities_use_the_binary_units() {
        assert_eq!(
            Usage::new(128 * 1024_u64.pow(3), 0).capacity_label(),
            "128 GiB"
        );
        assert_eq!(Usage::new(1024_u64.pow(4), 0).capacity_label(), "1 TiB");
        assert_eq!(Usage::new(512 * 1024 * 1024, 0).capacity_label(), "512 MiB");
    }

    /// The footer and the size column divide by the same 1024, so they must
    /// not disagree about what to call the result.
    #[test]
    fn the_footer_and_the_size_column_name_units_alike() {
        let g = 1024_u64.pow(3);
        let footer = Usage::new(128 * g, 0).capacity_label();
        let column = crate::row_format::RowFormat::human_size(128 * g);
        assert_eq!(footer, column, "one figure must have one name");
    }

    /// Below ten the extra digit carries real information; at and above it,
    /// it is noise in a 10px caption.
    #[test]
    fn only_small_values_keep_a_decimal() {
        let g = 1024_u64.pow(3);
        assert_eq!(Usage::new(3 * g + g / 2, 0).capacity_label(), "3.5 GiB");
        assert_eq!(Usage::new(128 * g, 0).capacity_label(), "128 GiB");
    }

    #[test]
    fn plain_bytes_keep_their_unit() {
        assert_eq!(Usage::new(0, 0).capacity_label(), "0 B");
        assert_eq!(Usage::new(512, 0).capacity_label(), "512 B");
    }

    /// An absurd capacity saturates at the largest unit rather than running
    /// off the end of the table.
    #[test]
    fn an_enormous_capacity_saturates_at_terabytes() {
        let label = Usage::new(u64::MAX, 0).capacity_label();
        assert!(label.ends_with(" TiB"), "got {label}");
    }

    #[test]
    fn the_figures_read_used_over_total() {
        let g = 1024_u64.pow(3);
        assert_eq!(Usage::new(128 * g, 66 * g).figures(), "62 GiB / 128 GiB");
        assert_eq!(Usage::new(8 * g, 8 * g).figures(), "0 B / 8 GiB");
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
    }

    /// A path that does not exist has no figures to report, and that is a
    /// normal outcome rather than an error worth surfacing.
    #[test]
    fn a_missing_path_reports_nothing() {
        let path = std::path::Path::new("/nonexistent-for-files-bin-storage-test");
        assert!(Storage::of(path).is_none());
    }

    #[test]
    fn a_subdirectory_reports_its_filesystem_total() {
        let dir = std::env::temp_dir();
        let nested = dir.join("files-bin-storage-probe");
        std::fs::create_dir_all(&nested).expect("can create a temp subdirectory");
        let outer = Storage::of(&dir).expect("temp dir has a filesystem");
        let inner = Storage::of(&nested).expect("subdirectory has a filesystem");
        assert_eq!(outer.usage().total(), inner.usage().total());
        let _ = std::fs::remove_dir(&nested);
    }
}
