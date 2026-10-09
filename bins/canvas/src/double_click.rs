//! [`DoubleClick`] -- recognising a second click on the same spot.
//!
//! `platform::MultiClickTracker` already does this for the terminal, but it
//! keys on `(u16, u16)` *cells* and needs the compositor's millisecond
//! timestamp. The canvas has neither: its presses arrive as
//! `PointerEventKind::Press` with a pixel position and no clock. Rather than
//! distort that type, this is the same idea over the inputs the canvas
//! actually has.
//!
//! Time comes from the caller so the rule stays testable without sleeping.

use std::time::{Duration, Instant};

/// Tracks consecutive presses to spot a double-click.
#[derive(Debug)]
pub struct DoubleClick {
    /// When and where the previous press landed.
    last: Option<(Instant, (i32, i32))>,
    /// Whether the previous press already completed a double-click, so a
    /// third rapid click starts a fresh count instead of chaining.
    consumed: bool,
}

impl Default for DoubleClick {
    fn default() -> Self {
        Self::new()
    }
}

impl DoubleClick {
    /// Longest gap between two presses that still pairs them. The X11/GTK
    /// default, comfortably above a deliberate double-click.
    pub const INTERVAL: Duration = Duration::from_millis(400);

    /// How far the pointer may drift between the two presses, in physical
    /// pixels. A hand shifts slightly during a fast double-click, and a
    /// canvas has no cell grid to snap that away.
    pub const SLOP: i32 = 6;

    /// A tracker that has seen no presses.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            consumed: false,
        }
    }

    /// Forget the previous press, so the next one cannot pair with it.
    ///
    /// Called whenever the meaning of a click changes -- switching tools,
    /// say. Without it, the press that finished one gesture pairs with the
    /// first press of the next and is misread as a double-click.
    pub const fn reset(&mut self) {
        self.last = None;
        self.consumed = false;
    }

    /// Record a press at `at` and report whether it completed a
    /// double-click.
    pub fn press(&mut self, at: (i32, i32)) -> bool {
        self.press_at(Instant::now(), at)
    }

    /// [`Self::press`] with an explicit clock, for tests.
    pub fn press_at(&mut self, now: Instant, at: (i32, i32)) -> bool {
        let chained = self.last.is_some_and(|(when, where_)| {
            (at.0 - where_.0).abs() <= Self::SLOP
                && (at.1 - where_.1).abs() <= Self::SLOP
                && now.duration_since(when) <= Self::INTERVAL
        });
        let double = chained && !self.consumed;
        self.last = Some((now, at));
        self.consumed = double;
        double
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn the_first_press_is_never_a_double_click() {
        let mut d = DoubleClick::new();
        assert!(!d.press_at(Instant::now(), (10, 10)));
    }

    #[test]
    fn two_quick_presses_on_the_same_spot_are_a_double_click() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        assert!(!d.press_at(base, (10, 10)));
        assert!(d.press_at(at(base, 120), (10, 10)));
    }

    #[test]
    fn a_slow_second_press_is_two_single_clicks() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        assert!(!d.press_at(at(base, 900), (10, 10)));
    }

    #[test]
    fn a_small_drift_still_counts_as_a_double_click() {
        // A hand moves a little between two fast clicks.
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        assert!(d.press_at(at(base, 120), (10 + DoubleClick::SLOP, 10)));
    }

    #[test]
    fn a_press_that_lands_far_away_is_a_fresh_click() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        assert!(!d.press_at(at(base, 120), (200, 200)));
    }

    #[test]
    fn a_drift_past_the_slop_is_a_fresh_click() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        assert!(!d.press_at(at(base, 120), (10 + DoubleClick::SLOP + 1, 10)));
    }

    #[test]
    fn a_triple_click_does_not_report_two_double_clicks() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        assert!(!d.press_at(base, (10, 10)));
        assert!(d.press_at(at(base, 100), (10, 10)));
        assert!(
            !d.press_at(at(base, 200), (10, 10)),
            "the third click must start a fresh count"
        );
    }

    #[test]
    fn a_fourth_click_can_pair_with_the_third() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        d.press_at(at(base, 100), (10, 10));
        d.press_at(at(base, 200), (10, 10));
        assert!(d.press_at(at(base, 300), (10, 10)));
    }

    #[test]
    fn a_reset_stops_the_next_press_from_pairing() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        d.reset();
        assert!(
            !d.press_at(at(base, 100), (10, 10)),
            "a press after a reset must start a fresh count"
        );
    }

    #[test]
    fn a_press_exactly_on_the_interval_still_pairs() {
        let base = Instant::now();
        let mut d = DoubleClick::new();
        d.press_at(base, (10, 10));
        assert!(d.press_at(base + DoubleClick::INTERVAL, (10, 10)));
    }
}
