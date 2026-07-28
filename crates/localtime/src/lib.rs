//! Local wall-clock time via raw libc (`time(2)` + `localtime_r(3)`), not a
//! `date` shell-out and not a chrono dependency.
//!
//! `clock()`/`date()` used to run the `date` binary once per poll -- cheap
//! individually, but it's still a process spawn (fork/exec, a fresh
//! address space, PATH lookup) for something the kernel and libc already
//! hand you as a couple of syscalls. `localtime_r` also does the right
//! thing with `/etc/localtime`/`TZ` the same way `date` does (it's the
//! exact same libc timezone database under the hood), so this is a
//! behavior-preserving swap, not a "simplified" one.
//!
//! # Why this is its own crate
//!
//! It started inside `crates/statusbar`, where the clock widget was the only
//! caller. A second consumer then appeared: the freedesktop trash spec
//! requires a `DeletionDate` in **local** time with no timezone suffix, which
//! `std` cannot produce (it offers only `SystemTime`, i.e. UTC-relative). The
//! choice was to duplicate a raw `unsafe extern "C"` block into
//! `crates/fileops` or to give it one owner -- and duplicated FFI is precisely
//! the thing that must not be copy-pasted.

pub mod timestamp;

pub use timestamp::Timestamp;

use std::os::raw::c_long;

// Field names are ours, not libc's -- only the layout (type + order) has
// to match `struct tm`, so the redundant `tm_` prefix libc uses (needed
// there since C has no namespacing) is dropped here rather than suppressing
// clippy's `struct_field_names` lint over it.
#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: c_long,
    zone: *const i8,
}

impl Default for Tm {
    fn default() -> Self {
        // SAFETY: an all-zero `Tm` is a valid (if not meaningful) bit
        // pattern for this struct -- every field is a plain integer or a
        // (possibly null) pointer.
        unsafe { std::mem::zeroed() }
    }
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, result: *mut Tm) -> *mut Tm;
}

/// Raw libc local-time reads -- a marker type rather than a value: every
/// call re-reads the current time, there's nothing to cache.
pub struct LocalTime;

impl LocalTime {
    /// Read the current local time as a [`Timestamp`].
    ///
    /// `None` only if libc's `localtime_r` fails, which in practice means a
    /// corrupt timezone database.
    #[must_use]
    pub fn now() -> Option<Timestamp> {
        let mut t: i64 = 0;
        // SAFETY: `&raw mut t` is a valid, aligned `*mut i64` for the
        // duration of this call.
        unsafe { time(&raw mut t) };
        Self::at_unix_seconds(t)
    }

    /// Convert a Unix timestamp to local civil time.
    ///
    /// Split out from [`Self::now`] so the conversion is testable with a
    /// *fixed* input -- otherwise every assertion about formatting would be
    /// racing the wall clock.
    #[must_use]
    pub fn at_unix_seconds(unix_seconds: i64) -> Option<Timestamp> {
        let t = unix_seconds;
        let mut tm = Tm::default();
        // SAFETY: `&raw const t` and `&raw mut tm` are both valid for the
        // duration of this call; `localtime_r` writes into `tm` and returns
        // a pointer aliasing it (or null on failure) rather than
        // allocating.
        let res = unsafe { localtime_r(&raw const t, &raw mut tm) };
        if res.is_null() {
            return None;
        }
        Some(Timestamp {
            // libc's `tm_year` counts from 1900 and `tm_mon` from 0; both
            // are normalised here so no caller has to remember the offsets.
            year: tm.year + 1900,
            month: tm.mon + 1,
            day: tm.mday,
            hour: tm.hour,
            minute: tm.min,
            second: tm.sec,
        })
    }

    /// Wall-clock `HH:MM:SS`, local time.
    #[must_use]
    pub fn clock() -> Option<String> {
        Some(Self::now()?.clock())
    }

    /// Date, `YYYY-MM-DD`.
    #[must_use]
    pub fn date() -> Option<String> {
        Some(Self::now()?.date())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_matches_expected_shape() {
        let s = LocalTime::clock().expect("clock() should succeed on any normal box");
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
    }

    #[test]
    fn date_matches_expected_shape() {
        let s = LocalTime::date().expect("date() should succeed on any normal box");
        assert_eq!(s.len(), 10);
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[7], b'-');
        // Sanity: year starts with "20" for the foreseeable future -- not
        // asserting an exact date since this test runs whenever it runs.
        assert!(s.starts_with("20"));
    }

    #[test]
    fn the_epoch_converts_without_failing() {
        // Whatever timezone the test box is in, the epoch is a valid instant
        // and must convert -- guards against a null-return path regression.
        let ts = LocalTime::at_unix_seconds(0).expect("epoch is representable");
        assert!(
            ts.year == 1969 || ts.year == 1970,
            "epoch lands in 1969 or 1970 depending on UTC offset, got {}",
            ts.year
        );
    }

    #[test]
    fn fields_are_normalised_into_human_ranges() {
        let ts = LocalTime::now().expect("now() should succeed");
        assert!(
            ts.year >= 2020,
            "year must be +1900-adjusted, got {}",
            ts.year
        );
        assert!(
            (1..=12).contains(&ts.month),
            "month must be 1-12, got {}",
            ts.month
        );
        assert!((1..=31).contains(&ts.day));
        assert!((0..=23).contains(&ts.hour));
        assert!((0..=59).contains(&ts.minute));
        // Leap seconds can legitimately yield 60.
        assert!((0..=60).contains(&ts.second));
    }

    #[test]
    fn agrees_with_the_system_date_command() {
        // Cross-check against `date` itself once, as a belt-and-braces
        // sanity check that the libc call and the shell-out it replaced
        // actually produce the same answer (allowing a day/second of
        // drift for a `date`-spawn racing a second boundary).
        let Ok(out) = std::process::Command::new("date").arg("+%Y-%m-%d").output() else {
            return; // no `date` on PATH in this environment -- skip
        };
        let expected = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ours = LocalTime::date().expect("date() should succeed");
        assert_eq!(ours, expected, "libc localtime disagrees with `date`");
    }
}
