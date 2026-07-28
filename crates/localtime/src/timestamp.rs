//! A civil-time instant (year/month/day, hour/minute/second) and the string
//! formats this workspace needs from it.
//!
//! Separate from the FFI in `lib.rs` on purpose: this type contains no
//! `unsafe`, so all the formatting rules are testable with fixed inputs
//! rather than racing the wall clock.

/// A local civil date and time, already normalised out of libc's `struct tm`
/// conventions (`year` is the full calendar year, `month` is 1-12).
///
/// Deliberately *not* timezone-aware: it represents "what a wall clock in
/// this machine's timezone reads", which is exactly what both consumers want
/// -- a status bar showing the time, and a `.trashinfo` `DeletionDate`, which
/// the freedesktop spec defines as local time with no offset suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
}

impl Timestamp {
    /// Wall-clock `HH:MM:SS`.
    #[must_use]
    pub fn clock(&self) -> String {
        format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }

    /// Date as `YYYY-MM-DD`.
    #[must_use]
    pub fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// The freedesktop trash spec's `DeletionDate` format:
    /// `YYYY-MM-DDThh:mm:ss`, local time, **no** timezone suffix.
    ///
    /// Verified against real `.trashinfo` files written by other tools on
    /// this machine, e.g. `DeletionDate=2026-07-27T20:04:50`. The `T`
    /// separator and the absence of a `Z`/offset are both load-bearing: a
    /// trailing `Z` would claim UTC and make every restored file's recorded
    /// deletion time wrong by the local offset.
    #[must_use]
    pub fn trash_deletion_date(&self) -> String {
        format!("{}T{}", self.date(), self.clock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant so these assertions never race the clock.
    const SAMPLE: Timestamp = Timestamp {
        year: 2026,
        month: 7,
        day: 27,
        hour: 20,
        minute: 4,
        second: 50,
    };

    #[test]
    fn formats_a_clock() {
        assert_eq!(SAMPLE.clock(), "20:04:50");
    }

    #[test]
    fn formats_a_date() {
        assert_eq!(SAMPLE.date(), "2026-07-27");
    }

    /// Byte-for-byte against a real `.trashinfo` line found on this machine.
    #[test]
    fn formats_the_trash_deletion_date_exactly_as_other_tools_do() {
        assert_eq!(SAMPLE.trash_deletion_date(), "2026-07-27T20:04:50");
    }

    #[test]
    fn deletion_date_carries_no_timezone_suffix() {
        let s = SAMPLE.trash_deletion_date();
        assert!(
            !s.ends_with('Z') && !s.contains('+'),
            "the spec's DeletionDate is local time with no offset: {s}"
        );
    }

    #[test]
    fn single_digit_fields_are_zero_padded() {
        let ts = Timestamp {
            year: 999,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            second: 5,
        };
        // Zero-padding matters because these strings get compared and sorted
        // as text; an unpadded month would sort "10" before "2".
        assert_eq!(ts.trash_deletion_date(), "0999-01-02T03:04:05");
    }

    #[test]
    fn timestamps_order_chronologically() {
        let earlier = Timestamp {
            second: 49,
            ..SAMPLE
        };
        assert!(earlier < SAMPLE, "derived Ord must be chronological");
    }
}
