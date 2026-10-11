//! [`DragBadge`] -- the "you are dragging these files" indicator.
//!
//! # Why this exists
//!
//! `wl_data_device.start_drag` takes an optional drag icon surface. This
//! window passes none, so the compositor shows the ordinary pointer and the
//! gesture becomes invisible: the user presses, moves, and sees nothing
//! change until the drop either works or does not. Worse, once the drag grab
//! begins the surface stops receiving `wl_pointer` events entirely, so the
//! row highlight the pointer was leaving behind freezes wherever it was.
//!
//! # Why a badge rather than a real drag icon
//!
//! A drag icon is a second `wl_surface` with its own buffer, its own
//! attach/damage/commit cycle, and its own lifetime tied to the data source.
//! That is a platform-layer feature, not a painter feature. The badge is
//! drawn inside the window we already paint, which costs nothing extra and
//! answers the actual question the user has: *what am I holding?*
//!
//! It is deliberately anchored to the drag's own position rather than a
//! fixed corner. A label in a corner reads as a status message; a label that
//! follows the pointer reads as cargo.

use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

use tiles::spacing::Spacing;

/// The colours and rhythm the badge draws itself with.
///
/// Grouped rather than passed loose because they travel together and mean
/// nothing apart: a fill without its border is not a badge, it is a smear.
#[derive(Clone, Copy)]
pub struct BadgeStyle {
    pub spacing: Spacing,
    pub fill: Color,
    pub border: Color,
    pub fg: Color,
}

/// The floating count of files a live drag is carrying.
pub struct DragBadge {
    /// How many paths the gesture picked up. Zero means no drag is live.
    count: usize,
    /// Where the drag currently is, in window coordinates.
    at: (i32, i32),
}

impl DragBadge {
    /// An idle badge: nothing is being dragged.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            count: 0,
            at: (0, 0),
        }
    }

    /// Record that a drag of `count` files started at `at`.
    pub const fn begin(&mut self, count: usize, at: (i32, i32)) {
        self.count = count;
        self.at = at;
    }

    /// Move the badge to `at`, if a drag is live.
    pub const fn follow(&mut self, at: (i32, i32)) {
        if self.count > 0 {
            self.at = at;
        }
    }

    /// Forget the drag -- the drop landed, or the gesture was abandoned.
    pub const fn end(&mut self) {
        self.count = 0;
    }

    /// Whether a drag is currently being carried.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.count > 0
    }

    /// The label the badge shows.
    #[must_use]
    fn label(&self) -> String {
        if self.count == 1 {
            "1 item".to_owned()
        } else {
            format!("{} items", self.count)
        }
    }

    /// Draw the badge near the drag position, clamped inside `bounds`.
    ///
    /// Offset from the pointer rather than centred on it, so the badge never
    /// covers the row the user is aiming at -- which is precisely the row
    /// they need to see to aim.
    pub fn paint(
        &self,
        canvas: &mut Canvas<'_>,
        text: &mut TextRenderer,
        bounds: Rect,
        style: BadgeStyle,
    ) {
        if self.count == 0 {
            return;
        }
        let label = self.label();
        let pad = style.spacing.s();
        let w = text.measure(&label) + pad * 2;
        let h = text.line_height() + pad;
        let offset = style.spacing.m();
        let x = (self.at.0 + offset)
            .min(bounds.x + bounds.w - w)
            .max(bounds.x);
        let y = (self.at.1 + offset)
            .min(bounds.y + bounds.h - h)
            .max(bounds.y);
        let rect = Rect::new(x, y, w, h);
        let radius = style.spacing.radius();
        canvas.fill_round_rect(rect, style.fill, radius);
        canvas.draw_round_rect(rect, style.border, radius);
        text.draw_text(canvas, &label, x + pad, y + pad / 2, style.fg, style.fill);
    }
}

impl Default for DragBadge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::DragBadge;

    #[test]
    fn a_fresh_badge_carries_nothing() {
        let badge = DragBadge::new();
        assert!(!badge.is_active());
    }

    #[test]
    fn beginning_a_drag_makes_it_active() {
        let mut badge = DragBadge::new();
        badge.begin(3, (10, 20));
        assert!(badge.is_active());
        assert_eq!(badge.label(), "3 items");
    }

    /// A count of one must not read "1 items", which looks like a bug to
    /// every user who sees it.
    #[test]
    fn a_single_file_is_labelled_in_the_singular() {
        let mut badge = DragBadge::new();
        badge.begin(1, (0, 0));
        assert_eq!(badge.label(), "1 item");
    }

    #[test]
    fn ending_a_drag_clears_it() {
        let mut badge = DragBadge::new();
        badge.begin(2, (0, 0));
        badge.end();
        assert!(!badge.is_active());
    }

    #[test]
    fn following_moves_a_live_badge() {
        let mut badge = DragBadge::new();
        badge.begin(1, (0, 0));
        badge.follow((50, 60));
        assert_eq!(badge.at, (50, 60));
    }

    /// Position updates arrive from the event loop every tick, including
    /// ticks where no drag is live. Those must not resurrect a dead badge.
    #[test]
    fn following_an_idle_badge_does_not_start_one() {
        let mut badge = DragBadge::new();
        badge.follow((50, 60));
        assert!(!badge.is_active());
        assert_eq!(badge.at, (0, 0));
    }
}
