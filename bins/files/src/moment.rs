//! [`Moment`] -- the `modified` column's relative time strings.
//!
//! The design guide's mockup shows `today 14:22`, `yesterday`, `jul 26`: a
//! recency-first scale, because the question a file list answers is "did I
//! touch this recently", not "what was the exact calendar date". A raw
//! `2026-07-26 14:22:03` in every row is both wider and harder to scan.
//!
//! The rules, coarsest-relevant-unit first:
//!
//! - today -> `today HH:MM`, since the time of day is what distinguishes
//!   this morning's work from this afternoon's
//! - yesterday -> `yesterday`
//! - within this calendar year -> `mon DD` (`jul 26`)
//! - older -> `mon YYYY` (`jul 2024`), because the year matters more than the
//!   day once something is that old
//!
//! Every function takes `now` as an argument rather than reading the clock.
//! That is the whole reason this is its own module: a formatter that calls
//! `SystemTime::now()` internally can only be tested by racing midnight,
//! so in practice it never gets tested at all.

use localtime::{LocalTime, Timestamp};
use std::time::SystemTime;

/// Formats modification times for display.
#[derive(Debug, Clone, Copy, Default)]
pub struct Moment;

impl Moment {
    /// Lowercase three-letter month names, matching the guide's terminal-ish
    /// lowercase voice.
    const MONTHS: [&'static str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    /// The em-dash shown when a filesystem reports no mtime.
    ///
    /// A dash rather than a blank: an empty cell looks like a rendering bug,
    /// while a dash reads as "deliberately nothing".
    pub const UNKNOWN: &'static str = "--";

    /// Format `when` relative to `now`.
    ///
    /// Returns [`Self::UNKNOWN`] when the entry has no mtime, or when the
    /// timestamp cannot be converted -- a file dated year 200000 must render
    /// as unknown rather than panicking the paint loop.
    #[must_use]
    pub fn relative(when: Option<SystemTime>, now: Timestamp) -> String {
        let Some(stamp) = when.and_then(Self::civil) else {
            return Self::UNKNOWN.to_owned();
        };
        Self::describe(stamp, now)
    }

    /// Convert a `SystemTime` to local civil time.
    ///
    /// Times before the epoch are real (archives, deliberately backdated
    /// files) and are handled by taking the signed offset rather than
    /// clamping, so a 1969 file keeps its actual date.
    fn civil(when: SystemTime) -> Option<Timestamp> {
        let seconds = match when.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(after) => i64::try_from(after.as_secs()).ok()?,
            // `duration_since` errs for pre-epoch times and carries the
            // magnitude in the error, so a negative instant is recoverable.
            Err(before) => -i64::try_from(before.duration().as_secs()).ok()?,
        };
        LocalTime::at_unix_seconds(seconds)
    }

    /// The relative description of `stamp` as of `now`.
    ///
    /// Split from [`Self::relative`] so the rules can be tested against fixed
    /// civil times with no `SystemTime` conversion in the way.
    #[must_use]
    pub fn describe(stamp: Timestamp, now: Timestamp) -> String {
        if Self::same_day(stamp, now) {
            return format!("today {:02}:{:02}", stamp.hour, stamp.minute);
        }
        if Self::is_yesterday(stamp, now) {
            return "yesterday".to_owned();
        }
        let month = Self::month_name(stamp.month);
        if stamp.year == now.year {
            format!("{month} {:02}", stamp.day)
        } else {
            // Past the current year the day stops being useful and the year
            // starts being the thing you actually want to know.
            format!("{month} {}", stamp.year)
        }
    }

    /// A lowercase three-letter month, or `"?"` for an out-of-range value.
    ///
    /// Defensive because `month` arrives from libc via FFI; an out-of-range
    /// index must not panic inside a paint callback.
    fn month_name(month: i32) -> &'static str {
        // `checked_sub` rather than `month - 1`: an out-of-range value from
        // FFI can be `i32::MIN`, where the subtraction itself overflows and
        // panics before the range check ever runs.
        month
            .checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| Self::MONTHS.get(i))
            .copied()
            .unwrap_or("?")
    }

    /// Whether two civil times fall on the same calendar day.
    const fn same_day(a: Timestamp, b: Timestamp) -> bool {
        a.year == b.year && a.month == b.month && a.day == b.day
    }

    /// Whether `stamp` is the calendar day before `now`.
    ///
    /// Compares day *numbers* rather than subtracting seconds, so it is
    /// correct across a DST change: "yesterday" is a calendar concept, and a
    /// 23-hour day must not make yesterday evening read as today.
    fn is_yesterday(stamp: Timestamp, now: Timestamp) -> bool {
        Self::day_number(stamp) + 1 == Self::day_number(now)
    }

    /// Days since a fixed epoch, for calendar-day arithmetic.
    ///
    /// This is the standard days-from-civil algorithm (Howard Hinnant's),
    /// which is exact for the whole proleptic Gregorian calendar and, unlike
    /// a naive month-length table, handles leap years and month boundaries
    /// without a special case per month.
    fn day_number(t: Timestamp) -> i64 {
        let y = i64::from(t.year);
        let m = i64::from(t.month);
        let d = i64::from(t.day);
        // Shift the year to start in March, which puts the leap day at the
        // end of the year and removes February's special case entirely.
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let year_of_era = y - era * 400;
        let month_shifted = if m > 2 { m - 3 } else { m + 9 };
        let day_of_year = (153 * month_shifted + 2) / 5 + d - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }
}

#[cfg(test)]
mod tests {
    use super::Moment;
    use localtime::Timestamp;

    /// A fixed "now" so no assertion here can race the wall clock.
    const NOW: Timestamp = Timestamp {
        year: 2026,
        month: 7,
        day: 28,
        hour: 16,
        minute: 30,
        second: 0,
    };

    fn at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Timestamp {
        Timestamp {
            year,
            month,
            day,
            hour,
            minute,
            second: 0,
        }
    }

    /// Today shows the clock time: distinguishing this morning's edit from
    /// this afternoon's is the whole point.
    #[test]
    fn today_shows_the_time_of_day() {
        assert_eq!(
            Moment::describe(at(2026, 7, 28, 14, 22), NOW),
            "today 14:22"
        );
        assert_eq!(Moment::describe(at(2026, 7, 28, 0, 5), NOW), "today 00:05");
    }

    #[test]
    fn yesterday_is_named_rather_than_dated() {
        assert_eq!(Moment::describe(at(2026, 7, 27, 23, 59), NOW), "yesterday");
    }

    /// Yesterday across a month boundary must still read as yesterday --
    /// a day-of-month comparison would break here.
    #[test]
    fn yesterday_works_across_a_month_boundary() {
        let now = at(2026, 8, 1, 9, 0);
        assert_eq!(Moment::describe(at(2026, 7, 31, 18, 0), now), "yesterday");
    }

    /// ...and across a year boundary, where naive arithmetic fails worst.
    #[test]
    fn yesterday_works_across_a_year_boundary() {
        let now = at(2026, 1, 1, 9, 0);
        assert_eq!(Moment::describe(at(2025, 12, 31, 18, 0), now), "yesterday");
    }

    /// Leap day is a real yesterday, which a 28-day February table would miss.
    #[test]
    fn yesterday_works_across_a_leap_day() {
        let now = at(2024, 3, 1, 9, 0);
        assert_eq!(Moment::describe(at(2024, 2, 29, 18, 0), now), "yesterday");
    }

    #[test]
    fn earlier_this_year_shows_month_and_day() {
        assert_eq!(Moment::describe(at(2026, 7, 26, 11, 47), NOW), "jul 26");
        assert_eq!(Moment::describe(at(2026, 1, 3, 11, 47), NOW), "jan 03");
    }

    /// Once something is more than a year old the year matters more than the
    /// day of the month.
    #[test]
    fn older_entries_show_the_year_instead_of_the_day() {
        assert_eq!(Moment::describe(at(2024, 7, 26, 11, 47), NOW), "jul 2024");
        assert_eq!(Moment::describe(at(1999, 12, 31, 0, 0), NOW), "dec 1999");
    }

    /// A future mtime is real (bad clocks, tarballs, network filesystems) and
    /// must render as *something* rather than panicking or claiming today.
    #[test]
    fn a_future_timestamp_still_formats() {
        let out = Moment::describe(at(2030, 3, 4, 12, 0), NOW);
        assert_eq!(out, "mar 2030");
        // Later today is still today.
        assert_eq!(Moment::describe(at(2026, 7, 28, 23, 0), NOW), "today 23:00");
    }

    /// Tomorrow must not be mistaken for yesterday -- the comparison is
    /// directional and an `abs()` would get this wrong.
    #[test]
    fn tomorrow_is_not_yesterday() {
        let out = Moment::describe(at(2026, 7, 29, 10, 0), NOW);
        assert_eq!(out, "jul 29");
    }

    /// No mtime renders as a deliberate dash, not an empty cell that looks
    /// like a bug.
    #[test]
    fn a_missing_timestamp_is_marked_not_blank() {
        assert_eq!(Moment::relative(None, NOW), Moment::UNKNOWN);
        assert!(!Moment::UNKNOWN.is_empty());
    }

    /// An out-of-range month arriving from libc must not panic a paint call.
    #[test]
    fn a_nonsense_month_degrades_instead_of_panicking() {
        for month in [0, 13, -1, i32::MAX, i32::MIN] {
            let out = Moment::describe(at(2020, month, 1, 0, 0), NOW);
            assert!(!out.is_empty(), "month {month} produced nothing");
        }
    }

    /// The real `SystemTime` path must work end to end, not just `describe`.
    #[test]
    fn a_real_system_time_formats_as_today() {
        let today = localtime::LocalTime::now().expect("local time must be available");
        let out = Moment::relative(Some(std::time::SystemTime::now()), today);
        assert!(
            out.starts_with("today "),
            "now should read as today, got {out}"
        );
    }

    /// The epoch itself is a real mtime on plenty of files.
    ///
    /// Asserted loosely on purpose: the epoch is *local* midnight only in
    /// UTC, and west of it the same instant falls on 1969-12-31. Pinning
    /// "jan 1970" here would be a test that passes in London and fails in
    /// this timezone.
    #[test]
    fn the_epoch_formats_rather_than_failing() {
        let out = Moment::relative(Some(std::time::SystemTime::UNIX_EPOCH), NOW);
        assert!(
            out.ends_with("1970") || out.ends_with("1969"),
            "expected an epoch-adjacent year, got {out}"
        );
    }

    /// A pre-epoch mtime must keep its real date rather than being clamped
    /// to 1970 -- `duration_since` errs for these and the magnitude is in the
    /// error, which is easy to get wrong.
    #[test]
    fn a_pre_epoch_time_keeps_its_own_date() {
        let when = std::time::SystemTime::UNIX_EPOCH - std::time::Duration::from_hours(24 * 400);
        let out = Moment::relative(Some(when), NOW);
        assert!(out.ends_with("1968"), "expected a 1968 date, got {out}");
    }

    /// An absurd `SystemTime` must degrade to unknown rather than panicking
    /// or producing a nonsense string.
    ///
    /// Built with `checked_add` because the platform's `SystemTime` cannot
    /// even represent some of these: the point is that *our* code survives
    /// whatever the platform hands us, so a value the platform refuses to
    /// construct is simply not a case we can be handed.
    #[test]
    fn an_absurd_timestamp_degrades_to_unknown() {
        let absurd = std::time::Duration::from_secs(u64::MAX / 2);
        let Some(when) = std::time::SystemTime::UNIX_EPOCH.checked_add(absurd) else {
            return;
        };
        let out = Moment::relative(Some(when), NOW);
        assert_eq!(out, Moment::UNKNOWN);
    }
}
