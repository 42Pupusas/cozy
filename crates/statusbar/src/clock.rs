//! Wall-clock time-of-day + a cheap monotonic-ish tick, on top of
//! `crate::localtime`'s raw libc reads.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value:
//! every call re-reads the current time, there's nothing to cache.

use std::time::{SystemTime, UNIX_EPOCH};

/// Clock/date/tick reads -- see the module doc for why this is a marker
/// type.
pub struct Clock;

impl Clock {
    /// Wall-clock `HH:MM:SS`, local time — via `crate::localtime` (raw
    /// `time(2)`/`localtime_r(3)`), not a `date` shell-out.
    #[must_use]
    pub fn time() -> Option<String> {
        crate::localtime::LocalTime::clock()
    }

    /// Date, `YYYY-MM-DD`.
    #[must_use]
    pub fn date() -> Option<String> {
        crate::localtime::LocalTime::date()
    }

    /// Unix timestamp in whole seconds — a cheap monotonic-ish tick used by
    /// the bar's refresh scheduler to decide "has at least N seconds
    /// passed".
    #[must_use]
    pub fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
