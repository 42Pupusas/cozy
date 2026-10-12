//! [`Handle`] -- the grab zones around a selected shape, and what the
//! pointer should look like over each one.
//!
//! Pure geometry: which part of a selection a world point grabs, and the
//! cursor that advertises it. The window turns [`CursorShape`] into a real
//! pointer image; nothing here knows that.
//!
//! # Why the zones are generous
//!
//! A resize corner drawn 8px wide is 8px wide to the eye but much smaller
//! to the hand, because the pointer must land inside it *before* the button
//! goes down. [`Handle::GRAB`] is therefore larger than the painted square,
//! and edges are grabbable along their whole length rather than only at
//! their midpoints.

pub use widget::CursorShape;

/// Which part of a selected shape a point grabs.
///
/// Corners come before edges in [`Handle::hit`] so that the overlapping
/// region at a corner resizes both axes, which is what a user aiming at a
/// corner means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    /// Inside the shape: move it rather than resize it.
    Body,
}

impl Handle {
    /// Half-width of a corner's grab square, in screen pixels. The painted
    /// square is smaller; this is the forgiving zone around it.
    pub const GRAB: f64 = 10.0;

    /// How far outside an edge still counts as grabbing that edge.
    pub const EDGE: f64 = 7.0;

    /// Every resize handle, corners first.
    pub const RESIZE: [Self; 8] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomRight,
        Self::BottomLeft,
        Self::Top,
        Self::Right,
        Self::Bottom,
        Self::Left,
    ];

    /// The cursor this handle asks for.
    #[must_use]
    pub const fn cursor(self) -> CursorShape {
        match self {
            Self::TopLeft | Self::BottomRight => CursorShape::ResizeNwse,
            Self::TopRight | Self::BottomLeft => CursorShape::ResizeNesw,
            Self::Left | Self::Right => CursorShape::ResizeEw,
            Self::Top | Self::Bottom => CursorShape::ResizeNs,
            Self::Body => CursorShape::Move,
        }
    }

    /// Whether this handle moves the shape's left edge.
    #[must_use]
    pub const fn moves_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::Left | Self::BottomLeft)
    }

    /// Whether this handle moves the shape's right edge.
    #[must_use]
    pub const fn moves_right(self) -> bool {
        matches!(self, Self::TopRight | Self::Right | Self::BottomRight)
    }

    /// Whether this handle moves the shape's top edge.
    #[must_use]
    pub const fn moves_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::Top | Self::TopRight)
    }

    /// Whether this handle moves the shape's bottom edge.
    #[must_use]
    pub const fn moves_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::Bottom | Self::BottomRight)
    }

    /// The anchor point of `handle` on the bounds `(x, y, w, h)`, in world
    /// coordinates: where its painted square sits.
    #[must_use]
    pub fn anchor(self, bounds: (f64, f64, f64, f64)) -> (f64, f64) {
        let (x, y, w, h) = bounds;
        let (mx, my) = (x + w / 2.0, y + h / 2.0);
        match self {
            Self::TopLeft => (x, y),
            Self::Top => (mx, y),
            Self::TopRight => (x + w, y),
            Self::Right => (x + w, my),
            Self::BottomRight => (x + w, y + h),
            Self::Bottom => (mx, y + h),
            Self::BottomLeft => (x, y + h),
            Self::Left => (x, my),
            Self::Body => (mx, my),
        }
    }

    /// Which handle `p` grabs on the shape bounded by `bounds`, or `None`
    /// when the point is outside the shape and all its handles.
    ///
    /// `scale` converts screen pixels to world units (pass `1.0 / zoom`) so
    /// the grab zones stay a constant size on screen at any zoom.
    #[must_use]
    pub fn hit(bounds: (f64, f64, f64, f64), p: (f64, f64), scale: f64) -> Option<Self> {
        let grab = Self::GRAB * scale;
        for handle in Self::RESIZE {
            let (ax, ay) = handle.anchor(bounds);
            if (p.0 - ax).abs() <= grab && (p.1 - ay).abs() <= grab {
                return Some(handle);
            }
        }
        Self::body_hit(bounds, p, scale).then_some(Self::Body)
    }

    /// Whether `p` is within the shape's body, widened by the edge slop so
    /// a near-miss on the outline still grabs it.
    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub fn body_hit(bounds: (f64, f64, f64, f64), p: (f64, f64), scale: f64) -> bool {
        let (x, y, w, h) = bounds;
        let pad = Self::EDGE * scale;
        p.0 >= x - pad && p.0 <= x + w + pad && p.1 >= y - pad && p.1 <= y + h + pad
    }

    /// Apply this handle's drag to `bounds`, moving only the edges it owns.
    ///
    /// Returns the new bounds, normalised so width and height stay
    /// positive: dragging a corner past its opposite flips the rectangle
    /// rather than inverting it. `min` is the smallest allowed extent.
    #[must_use]
    pub fn resize(
        self,
        bounds: (f64, f64, f64, f64),
        to: (f64, f64),
        min: f64,
    ) -> (f64, f64, f64, f64) {
        let (x, y, w, h) = bounds;
        let (mut left, mut top) = (x, y);
        let (mut right, mut bottom) = (x + w, y + h);
        if self.moves_left() {
            left = to.0;
        }
        if self.moves_right() {
            right = to.0;
        }
        if self.moves_top() {
            top = to.1;
        }
        if self.moves_bottom() {
            bottom = to.1;
        }
        let (nx, nw) = Self::span(left, right, min);
        let (ny, nh) = Self::span(top, bottom, min);
        (nx, ny, nw, nh)
    }

    /// Normalise one axis of a resize into `(origin, extent)`, keeping the
    /// extent at least `min` and never negative.
    fn span(a: f64, b: f64, min: f64) -> (f64, f64) {
        let origin = a.min(b);
        let extent = (b - a).abs().max(min);
        (origin, extent)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// A 100x50 rectangle at the origin, used by most tests.
    const BOUNDS: (f64, f64, f64, f64) = (0.0, 0.0, 100.0, 50.0);

    #[test]
    fn every_handle_anchors_somewhere_on_the_bounds() {
        for handle in Handle::RESIZE {
            let (ax, ay) = handle.anchor(BOUNDS);
            assert!((0.0..=100.0).contains(&ax), "{handle:?} x escaped: {ax}");
            assert!((0.0..=50.0).contains(&ay), "{handle:?} y escaped: {ay}");
        }
    }

    #[test]
    fn the_corners_sit_at_the_four_corners() {
        assert_eq!(Handle::TopLeft.anchor(BOUNDS), (0.0, 0.0));
        assert_eq!(Handle::TopRight.anchor(BOUNDS), (100.0, 0.0));
        assert_eq!(Handle::BottomRight.anchor(BOUNDS), (100.0, 50.0));
        assert_eq!(Handle::BottomLeft.anchor(BOUNDS), (0.0, 50.0));
    }

    #[test]
    fn the_edges_sit_at_the_midpoints() {
        assert_eq!(Handle::Top.anchor(BOUNDS), (50.0, 0.0));
        assert_eq!(Handle::Bottom.anchor(BOUNDS), (50.0, 50.0));
        assert_eq!(Handle::Left.anchor(BOUNDS), (0.0, 25.0));
        assert_eq!(Handle::Right.anchor(BOUNDS), (100.0, 25.0));
    }

    #[test]
    fn a_corner_is_grabbable_from_outside_the_shape() {
        // Up and left of the top-left corner: outside the rect entirely.
        let hit = Handle::hit(BOUNDS, (-6.0, -6.0), 1.0);
        assert_eq!(hit, Some(Handle::TopLeft));
    }

    #[test]
    fn the_grab_zone_is_larger_than_the_painted_square() {
        // The painted handle is 8px; 9px out must still grab it.
        let hit = Handle::hit(BOUNDS, (109.0, 50.0), 1.0);
        assert_eq!(hit, Some(Handle::BottomRight), "grab zone too tight");
    }

    #[test]
    fn a_corner_outranks_the_edge_it_overlaps() {
        // Exactly on the top-left corner: both TopLeft and Top/Left are in
        // range, and the corner must win.
        assert_eq!(Handle::hit(BOUNDS, (0.0, 0.0), 1.0), Some(Handle::TopLeft));
    }

    #[test]
    fn the_interior_grabs_the_body() {
        assert_eq!(Handle::hit(BOUNDS, (50.0, 25.0), 1.0), Some(Handle::Body));
    }

    #[test]
    fn a_point_far_outside_grabs_nothing() {
        assert_eq!(Handle::hit(BOUNDS, (500.0, 500.0), 1.0), None);
    }

    #[test]
    fn zooming_out_widens_the_world_space_grab_zone() {
        // At 0.5x zoom one screen pixel is two world units, so a point that
        // misses at 1.0 scale is caught at 2.0 scale.
        let far = (118.0, 50.0);
        assert_eq!(Handle::hit(BOUNDS, far, 1.0), None);
        assert_eq!(Handle::hit(BOUNDS, far, 2.0), Some(Handle::BottomRight));
    }

    #[test]
    fn each_handle_asks_for_a_sensible_cursor() {
        assert_eq!(Handle::TopLeft.cursor(), CursorShape::ResizeNwse);
        assert_eq!(Handle::BottomRight.cursor(), CursorShape::ResizeNwse);
        assert_eq!(Handle::TopRight.cursor(), CursorShape::ResizeNesw);
        assert_eq!(Handle::BottomLeft.cursor(), CursorShape::ResizeNesw);
        assert_eq!(Handle::Left.cursor(), CursorShape::ResizeEw);
        assert_eq!(Handle::Right.cursor(), CursorShape::ResizeEw);
        assert_eq!(Handle::Top.cursor(), CursorShape::ResizeNs);
        assert_eq!(Handle::Bottom.cursor(), CursorShape::ResizeNs);
        assert_eq!(Handle::Body.cursor(), CursorShape::Move);
    }

    #[test]
    fn opposite_corners_share_a_diagonal_cursor() {
        assert_eq!(Handle::TopLeft.cursor(), Handle::BottomRight.cursor());
        assert_eq!(Handle::TopRight.cursor(), Handle::BottomLeft.cursor());
        assert_ne!(Handle::TopLeft.cursor(), Handle::TopRight.cursor());
    }

    #[test]
    fn dragging_the_right_edge_moves_only_that_edge() {
        let out = Handle::Right.resize(BOUNDS, (150.0, 999.0), 1.0);
        assert_eq!(out, (0.0, 0.0, 150.0, 50.0), "y must not move");
    }

    #[test]
    fn dragging_the_top_edge_moves_the_origin_not_just_the_height() {
        let out = Handle::Top.resize(BOUNDS, (999.0, -20.0), 1.0);
        assert_eq!(out, (0.0, -20.0, 100.0, 70.0));
    }

    #[test]
    fn dragging_the_top_left_corner_moves_both_origin_axes() {
        let out = Handle::TopLeft.resize(BOUNDS, (-10.0, -10.0), 1.0);
        assert_eq!(out, (-10.0, -10.0, 110.0, 60.0));
    }

    #[test]
    fn dragging_a_corner_past_its_opposite_flips_rather_than_inverts() {
        // Drag the bottom-right corner far up and left of the top-left one.
        let out = Handle::BottomRight.resize(BOUNDS, (-40.0, -30.0), 1.0);
        let (_, _, w, h) = out;
        assert!(w > 0.0 && h > 0.0, "extent went negative: {out:?}");
        assert_eq!(out, (-40.0, -30.0, 40.0, 30.0));
    }

    #[test]
    fn a_resize_never_goes_below_the_minimum() {
        let out = Handle::Right.resize(BOUNDS, (0.0, 0.0), 12.0);
        let (_, _, w, _) = out;
        assert_eq!(w, 12.0);
    }

    #[test]
    fn resizing_by_the_body_is_a_no_op_on_the_bounds() {
        // Body moves rather than resizes, so no edge is owned.
        assert_eq!(Handle::Body.resize(BOUNDS, (500.0, 500.0), 1.0), BOUNDS);
    }

    #[test]
    fn every_handle_owns_the_edges_its_name_claims() {
        assert!(Handle::TopLeft.moves_left() && Handle::TopLeft.moves_top());
        assert!(!Handle::TopLeft.moves_right() && !Handle::TopLeft.moves_bottom());
        assert!(Handle::Bottom.moves_bottom());
        assert!(!Handle::Bottom.moves_left() && !Handle::Bottom.moves_right());
        assert!(!Handle::Body.moves_left());
        assert!(!Handle::Body.moves_top());
    }
}
