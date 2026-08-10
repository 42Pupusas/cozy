//! Multi-click detection for pointer presses.
//!
//! Wayland gives us bare `wl_pointer.button` presses with a millisecond
//! timestamp and no notion of "this is the second click of a double-click" --
//! that classification is entirely the client's job. This module is that
//! classification and nothing else, so the window can ask one question
//! ("what kind of click was this?") instead of carrying two more fields and
//! a timing comparison inline.
//!
//! Deliberately separate from [`crate::selection`]: *when* clicks are a
//! double-click is a pointer-timing concern, while *what* a double-click
//! selects is a text concern. Keeping them apart means either can be tested
//! without simulating the other.

/// How many clicks in a row landed on the same cell within the threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickKind {
    /// A fresh click: too slow, or on a different cell than the last one.
    Single,
    /// The second click of a double-click -- selects the word under it.
    Double,
}

/// Tracks consecutive presses to classify them as single or double clicks.
///
/// The state is just "when and where was the previous press, and did it
/// already complete a double-click". That last flag matters: without it a
/// rapid triple-click would report `Double` twice (each click pairing with
/// its predecessor), making the third click re-select the same word after
/// the user may have started dragging from it.
#[derive(Default)]
pub struct MultiClickTracker {
    /// Timestamp (ms, compositor clock) and cell of the previous press, or
    /// `None` before the first press of the session.
    last: Option<(u32, (u16, u16))>,
    /// Whether the previous press was already classified as a `Double`, so
    /// the next one starts a fresh count instead of chaining.
    consumed: bool,
}

impl MultiClickTracker {
    /// Maximum gap between two presses for them to count as a double-click.
    ///
    /// 400ms is the long-standing X11/GTK default: comfortably above a
    /// deliberate double-click (~150-250ms for most users) yet short enough
    /// that two unrelated clicks on the same word aren't fused.
    const THRESHOLD_MS: u32 = 400;

    /// A tracker that has seen no clicks yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify a press at `cell` stamped `time_ms` (the compositor's
    /// `wl_pointer.button` time argument).
    ///
    /// The cell must match exactly: a double-click that drifts to a
    /// neighbouring cell is treated as two single clicks, which is what the
    /// user gets in every other terminal and avoids surprise word-selects
    /// during fast, sloppy clicking across a line.
    pub fn press(&mut self, time_ms: u32, cell: (u16, u16)) -> ClickKind {
        // `wrapping_sub` because the compositor's millisecond clock is a
        // free-running u32 that wraps roughly every 49 days; a wrap must not
        // produce a huge delta (breaking a legitimate double-click) nor a
        // panic in debug builds.
        let chained = self.last.is_some_and(|(prev_time, prev_cell)| {
            prev_cell == cell && time_ms.wrapping_sub(prev_time) <= Self::THRESHOLD_MS
        });
        let kind = if chained && !self.consumed {
            ClickKind::Double
        } else {
            ClickKind::Single
        };
        self.last = Some((time_ms, cell));
        self.consumed = kind == ClickKind::Double;
        kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_press_is_always_single() {
        let mut t = MultiClickTracker::new();
        assert_eq!(t.press(1000, (3, 4)), ClickKind::Single);
    }

    #[test]
    fn two_quick_presses_on_one_cell_are_a_double_click() {
        let mut t = MultiClickTracker::new();
        assert_eq!(t.press(1000, (3, 4)), ClickKind::Single);
        assert_eq!(t.press(1200, (3, 4)), ClickKind::Double);
    }

    #[test]
    fn a_slow_second_press_is_a_fresh_single_click() {
        let mut t = MultiClickTracker::new();
        t.press(1000, (3, 4));
        assert_eq!(t.press(1500, (3, 4)), ClickKind::Single);
    }

    #[test]
    fn a_second_press_on_another_cell_is_a_fresh_single_click() {
        let mut t = MultiClickTracker::new();
        t.press(1000, (3, 4));
        assert_eq!(t.press(1100, (4, 4)), ClickKind::Single);
    }

    #[test]
    fn a_third_quick_press_does_not_re_report_double() {
        let mut t = MultiClickTracker::new();
        t.press(1000, (3, 4));
        assert_eq!(t.press(1100, (3, 4)), ClickKind::Double);
        assert_eq!(t.press(1200, (3, 4)), ClickKind::Single);
        // ...and the count restarts cleanly from there.
        assert_eq!(t.press(1300, (3, 4)), ClickKind::Double);
    }

    #[test]
    fn a_clock_wrap_does_not_break_a_double_click() {
        let mut t = MultiClickTracker::new();
        t.press(u32::MAX - 50, (0, 0));
        assert_eq!(t.press(100, (0, 0)), ClickKind::Double);
    }
}
