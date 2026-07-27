//! The rubber-band drag state machine, with no Wayland in sight.
//!
//! Deliberately pure: it takes points and returns rectangles. Every rule that
//! is easy to get subtly wrong -- dragging right-to-left, a bare click that
//! selects nothing, a drag that leaves the output -- is decided here, where a
//! test can drive it directly, rather than inside the event loop where it
//! would need a live compositor to exercise.

use platform::draw::{Point, Rect};
use screencopy::CaptureRegion;

/// How a selection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionOutcome {
    /// Still selecting -- the button is down, or was never pressed.
    Pending,
    /// The user released on a region with real area.
    Committed(CaptureRegion),
    /// Escape, a right-click, or a release that enclosed no pixels. A bare
    /// click is treated as a cancel rather than a zero-pixel capture because
    /// "click to dismiss" is what a user reaching for Escape usually does.
    Cancelled,
}

/// Tracks one rubber-band drag from press to release.
///
/// The anchor is where the button went down; the cursor is where it is now.
/// Neither is stored normalised, because the *direction* of the drag has to
/// survive until the rectangle is actually needed -- normalising on the way in
/// would make an up-and-left drag indistinguishable from a down-and-right one
/// mid-gesture, and the painted band would jump.
pub struct DragSelection {
    /// Where the button went down; `None` until it does.
    anchor: Option<Point>,
    /// Latest pointer position, tracked whether or not a button is held so
    /// the overlay can show a crosshair before the drag starts.
    cursor: Point,
    outcome: SelectionOutcome,
    /// The surface's own size, used to clamp a drag that runs off the edge.
    bounds: Rect,
}

impl DragSelection {
    /// A fresh selection over a surface of `width` x `height` pixels.
    #[must_use]
    pub const fn new(width: i32, height: i32) -> Self {
        Self {
            anchor: None,
            cursor: Point::new(0, 0),
            outcome: SelectionOutcome::Pending,
            bounds: Rect::new(0, 0, width, height),
        }
    }

    /// Latest pointer position.
    #[must_use]
    pub const fn cursor(&self) -> Point {
        self.cursor
    }

    /// How the selection ended, if it has.
    #[must_use]
    pub const fn outcome(&self) -> SelectionOutcome {
        self.outcome
    }

    /// Whether the gesture is over, either way.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        !matches!(self.outcome, SelectionOutcome::Pending)
    }

    /// Button pressed: plant the anchor.
    pub const fn press(&mut self, at: Point) {
        self.cursor = at;
        self.anchor = Some(at);
    }

    /// Pointer moved: extend the band if one is being dragged.
    pub const fn motion(&mut self, to: Point) {
        self.cursor = to;
    }

    /// Button released: settle on a region, or cancel if it has no area.
    pub fn release(&mut self, at: Point) {
        self.cursor = at;
        if self.is_finished() {
            return;
        }
        self.outcome = match self.rect() {
            // A rectangle with real area is the whole point of the gesture.
            Some(rect) if rect.w > 0 && rect.h > 0 => SelectionOutcome::Committed(CaptureRegion {
                x: rect.x,
                y: rect.y,
                width: rect.w,
                height: rect.h,
            }),
            _ => SelectionOutcome::Cancelled,
        };
    }

    /// Escape, or any other "never mind".
    pub const fn cancel(&mut self) {
        self.outcome = SelectionOutcome::Cancelled;
    }

    /// The rectangle as it should be drawn (and captured) right now.
    ///
    /// Returns `None` before the first press, since there is nothing to band
    /// yet. The result is normalised so a drag in any of the four directions
    /// yields a positive-extent rect, then clipped to the surface so a drag
    /// that runs past the edge selects up to the edge instead of describing
    /// pixels the compositor would refuse to copy.
    #[must_use]
    pub fn rect(&self) -> Option<Rect> {
        let anchor = self.anchor?;
        let left = anchor.x.min(self.cursor.x);
        let top = anchor.y.min(self.cursor.y);
        let right = anchor.x.max(self.cursor.x);
        let bottom = anchor.y.max(self.cursor.y);
        // Clamp against the surface rather than the raw drag: the pointer can
        // legitimately be reported outside a layer surface (the compositor
        // keeps delivering motion during an implicit grab), and an
        // out-of-bounds capture region is a protocol error.
        let left = left.max(self.bounds.x);
        let top = top.max(self.bounds.y);
        let right = right.min(self.bounds.right());
        let bottom = bottom.min(self.bounds.bottom());
        Some(Rect::new(
            left,
            top,
            (right - left).max(0),
            (bottom - top).max(0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{DragSelection, SelectionOutcome};
    use platform::draw::Point;
    use screencopy::CaptureRegion;

    /// The ordinary gesture: press top-left, drag down-right, release.
    #[test]
    fn a_left_to_right_drag_selects_what_was_enclosed() {
        let mut selection = DragSelection::new(1920, 1080);
        selection.press(Point::new(100, 200));
        selection.motion(Point::new(400, 500));
        selection.release(Point::new(400, 500));
        assert_eq!(
            selection.outcome(),
            SelectionOutcome::Committed(CaptureRegion {
                x: 100,
                y: 200,
                width: 300,
                height: 300,
            })
        );
    }

    /// Dragging up-and-left is the same rectangle as dragging down-and-right
    /// between the same two corners -- the direction is a detail of how the
    /// user moved their hand, not of what they selected.
    #[test]
    fn dragging_backwards_selects_the_same_rectangle() {
        let mut backwards = DragSelection::new(1920, 1080);
        backwards.press(Point::new(400, 500));
        backwards.release(Point::new(100, 200));

        let mut forwards = DragSelection::new(1920, 1080);
        forwards.press(Point::new(100, 200));
        forwards.release(Point::new(400, 500));

        assert_eq!(backwards.outcome(), forwards.outcome());
    }

    /// A click with no drag encloses nothing; capturing a zero-pixel image
    /// would be a worse answer than saying "cancelled".
    #[test]
    fn a_bare_click_cancels_rather_than_capturing_nothing() {
        let mut selection = DragSelection::new(1920, 1080);
        selection.press(Point::new(640, 360));
        selection.release(Point::new(640, 360));
        assert_eq!(selection.outcome(), SelectionOutcome::Cancelled);
    }

    /// A drag thinner than a pixel on one axis still encloses no pixels.
    #[test]
    fn a_zero_height_drag_cancels() {
        let mut selection = DragSelection::new(1920, 1080);
        selection.press(Point::new(100, 300));
        selection.release(Point::new(500, 300));
        assert_eq!(selection.outcome(), SelectionOutcome::Cancelled);
    }

    /// Escape must win even mid-drag, with the button still down.
    #[test]
    fn cancelling_mid_drag_beats_a_later_release() {
        let mut selection = DragSelection::new(1920, 1080);
        selection.press(Point::new(100, 100));
        selection.motion(Point::new(500, 500));
        selection.cancel();
        selection.release(Point::new(500, 500));
        assert_eq!(selection.outcome(), SelectionOutcome::Cancelled);
        assert!(selection.is_finished());
    }

    /// The compositor keeps sending motion outside the surface during the
    /// implicit grab; the region handed to the protocol must still be inside
    /// the output or the capture is a protocol error.
    #[test]
    fn a_drag_past_the_edge_is_clamped_to_the_surface() {
        let mut selection = DragSelection::new(800, 600);
        selection.press(Point::new(700, 500));
        selection.release(Point::new(1200, 900));
        assert_eq!(
            selection.outcome(),
            SelectionOutcome::Committed(CaptureRegion {
                x: 700,
                y: 500,
                width: 100,
                height: 100,
            })
        );
    }

    /// Negative coordinates are equally possible during a grab.
    #[test]
    fn a_drag_past_the_top_left_is_clamped_too() {
        let mut selection = DragSelection::new(800, 600);
        selection.press(Point::new(100, 100));
        selection.release(Point::new(-300, -300));
        assert_eq!(
            selection.outcome(),
            SelectionOutcome::Committed(CaptureRegion {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
            })
        );
    }

    /// Before any press there is no band to paint.
    #[test]
    fn there_is_no_rectangle_before_the_first_press() {
        let selection = DragSelection::new(800, 600);
        assert!(selection.rect().is_none());
        assert_eq!(selection.outcome(), SelectionOutcome::Pending);
    }

    /// Mid-drag the band follows the cursor, so the overlay has something to
    /// paint on every motion event.
    #[test]
    fn the_band_follows_the_cursor_while_dragging() {
        let mut selection = DragSelection::new(1920, 1080);
        selection.press(Point::new(100, 100));
        selection.motion(Point::new(300, 250));
        let rect = selection.rect().expect("a press planted an anchor");
        assert_eq!((rect.x, rect.y, rect.w, rect.h), (100, 100, 200, 150));
        assert!(!selection.is_finished());
        assert_eq!(selection.outcome(), SelectionOutcome::Pending);
    }
}
