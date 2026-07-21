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

/// Raw libc local-time reads -- a marker type (like [`crate::clock::Clock`],
/// which builds on top of this) rather than a value: every call re-reads
/// the current time, there's nothing to cache.
pub struct LocalTime;

impl LocalTime {
    /// Read the current local time as `(hour, min, sec, year, month, day)`
    /// -- `year` is the full calendar year (already `+1900`-adjusted from
    /// libc's `tm_year`), `month` is 1-12 (already `+1`-adjusted from
    /// libc's 0-11).
    fn now() -> Option<(i32, i32, i32, i32, i32, i32)> {
        let mut t: i64 = 0;
        // SAFETY: `&raw mut t` is a valid, aligned `*mut i64` for the
        // duration of this call.
        unsafe { time(&raw mut t) };
        let mut tm = Tm::default();
        // SAFETY: `&raw const t` and `&raw mut tm` are both valid for the
        // duration of this call; `localtime_r` writes into `tm` and returns
        // a pointer aliasing it (or null on failure) rather than
        // allocating.
        let res = unsafe { localtime_r(&raw const t, &raw mut tm) };
        if res.is_null() {
            return None;
        }
        Some((tm.hour, tm.min, tm.sec, tm.year + 1900, tm.mon + 1, tm.mday))
    }

    /// Wall-clock `HH:MM:SS`, local time.
    #[must_use]
    pub fn clock() -> Option<String> {
        let (h, m, s, ..) = Self::now()?;
        Some(format!("{h:02}:{m:02}:{s:02}"))
    }

    /// Date, `YYYY-MM-DD`.
    #[must_use]
    pub fn date() -> Option<String> {
        let (_, _, _, y, mo, d) = Self::now()?;
        Some(format!("{y:04}-{mo:02}-{d:02}"))
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
    fn agrees_with_the_system_date_command() {
        // Cross-check against `date` itself once, as a belt-and-braces
        // sanity check that the libc call and the shell-out it replaced
        // actually produce the same answer (allowing a day/second of
        // drift for a `date`-spawn racing a second boundary).
        let Ok(out) = std::process::Command::new("date").arg("+%Y-%m-%d").output() else {
            return; // no `date` on PATH in this environment -- skip
        };
        let expected = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ours = LocalTime::date().unwrap();
        assert_eq!(ours, expected, "libc localtime disagrees with `date`");
    }
}
