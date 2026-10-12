//! The deadlines that guarantee the picker cannot stay on screen forever.
//!
//! The idle deadline is reset by any input, so on its own it never fires
//! while the pointer moves over a picker that cannot be dismissed. The total
//! deadline is never reset and bounds the overlay's lifetime unconditionally.

use std::time::{Duration, Instant};

/// Why the overlay gave up.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Expiry {
    NeverConfigured,
    Idle,
    Total,
}

impl Expiry {
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NeverConfigured => {
                "the compositor never configured the selection overlay; giving up rather \
                 than leaving an invisible surface covering the screen"
            }
            Self::Idle => "selection timed out with no input; cancelled",
            Self::Total => "selection stayed open past its time limit; cancelled",
        }
    }
}

/// The overlay's creation time, last input time, and limits.
pub struct OverlayDeadlines {
    created: Instant,
    last_input: Instant,
}

impl OverlayDeadlines {
    const CONFIGURE: Duration = Duration::from_secs(5);
    const IDLE: Duration = Duration::from_secs(30);
    const TOTAL: Duration = Duration::from_secs(60);

    #[must_use]
    pub const fn starting_at(now: Instant) -> Self {
        Self {
            created: now,
            last_input: now,
        }
    }

    pub const fn input_at(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// The deadline that has passed at `now`, if any.
    #[must_use]
    pub fn expired(&self, now: Instant, configured: bool) -> Option<Expiry> {
        let alive = now.saturating_duration_since(self.created);
        if alive >= Self::TOTAL {
            return Some(Expiry::Total);
        }
        if !configured && alive >= Self::CONFIGURE {
            return Some(Expiry::NeverConfigured);
        }
        if now.saturating_duration_since(self.last_input) >= Self::IDLE {
            return Some(Expiry::Idle);
        }
        None
    }

    /// Milliseconds until the nearest deadline at `now`, at least 1.
    #[must_use]
    pub fn wait_ms(&self, now: Instant, configured: bool) -> i32 {
        let alive = now.saturating_duration_since(self.created);
        let idle = now.saturating_duration_since(self.last_input);
        let mut remaining = Self::TOTAL
            .saturating_sub(alive)
            .min(Self::IDLE.saturating_sub(idle));
        if !configured {
            remaining = remaining.min(Self::CONFIGURE.saturating_sub(alive));
        }
        i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::{Expiry, OverlayDeadlines};
    use std::time::{Duration, Instant};

    #[test]
    fn a_fresh_overlay_has_not_expired() {
        let start = Instant::now();
        let deadlines = OverlayDeadlines::starting_at(start);
        assert_eq!(deadlines.expired(start, false), None);
    }

    #[test]
    fn an_unconfigured_overlay_expires_after_five_seconds() {
        let start = Instant::now();
        let deadlines = OverlayDeadlines::starting_at(start);
        let later = start + Duration::from_secs(5);
        assert_eq!(
            deadlines.expired(later, false),
            Some(Expiry::NeverConfigured)
        );
        assert_eq!(deadlines.expired(later, true), None);
    }

    #[test]
    fn idle_time_expires_without_input() {
        let start = Instant::now();
        let deadlines = OverlayDeadlines::starting_at(start);
        let later = start + Duration::from_secs(30);
        assert_eq!(deadlines.expired(later, true), Some(Expiry::Idle));
    }

    #[test]
    fn input_resets_idle_time() {
        let start = Instant::now();
        let mut deadlines = OverlayDeadlines::starting_at(start);
        deadlines.input_at(start + Duration::from_secs(25));
        assert_eq!(
            deadlines.expired(start + Duration::from_secs(40), true),
            None
        );
    }

    #[test]
    fn constant_input_cannot_outlive_the_total_limit() {
        let start = Instant::now();
        let mut deadlines = OverlayDeadlines::starting_at(start);
        let end = start + Duration::from_secs(60);
        deadlines.input_at(end);
        assert_eq!(deadlines.expired(end, true), Some(Expiry::Total));
    }

    #[test]
    fn the_wait_is_bounded_by_the_nearest_deadline() {
        let start = Instant::now();
        let mut deadlines = OverlayDeadlines::starting_at(start);
        assert_eq!(deadlines.wait_ms(start, false), 5_000);
        assert_eq!(deadlines.wait_ms(start, true), 30_000);
        deadlines.input_at(start + Duration::from_secs(50));
        assert_eq!(
            deadlines.wait_ms(start + Duration::from_secs(50), true),
            10_000
        );
    }

    #[test]
    fn an_expired_wait_is_never_zero() {
        let start = Instant::now();
        let deadlines = OverlayDeadlines::starting_at(start);
        assert_eq!(deadlines.wait_ms(start + Duration::from_secs(90), true), 1);
    }
}
