//! [`RoundRect`] -- the per-row geometry of a rectangle with rounded corners.
//!
//! Every widget surface we paint is ultimately a run of horizontal spans, so a
//! rounded rectangle is fully described by "for each scanline, where does the
//! fill start and stop, and how much of the boundary pixel does it cover?".
//! Keeping that arithmetic here rather than inline in [`crate::draw::Canvas`]
//! is what makes it testable: the shape of a corner can be asserted directly,
//! at a dozen radii and degenerate sizes, without a framebuffer to read back
//! or a compositor to talk to.
//!
//! The coverage figures are what separate this from a stair-stepped
//! approximation. A corner quantised to whole pixels reads as a chewed edge at
//! the small radii UI chrome actually uses (4-6px), which looks worse than the
//! square corner it replaced -- so each span carries the fractional coverage
//! of the one boundary pixel at either end, and the canvas blends those two
//! rather than filling them.

use crate::draw::Rect;

/// One scanline of a rounded rectangle.
///
/// `x0..x1` is the fully covered run. The single pixel *before* `x0` and the
/// single pixel *at* `x1` are the anti-aliased boundary, covered
/// `left_alpha/255` and `right_alpha/255` respectively -- zero when the row
/// happens to land exactly on a pixel edge, which is the common case for every
/// row in the rectangle's straight middle section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub y: i32,
    pub x0: i32,
    pub x1: i32,
    pub left_alpha: u8,
    pub right_alpha: u8,
}

/// A rectangle with all four corners rounded to a common radius.
#[derive(Clone, Copy, Debug)]
pub struct RoundRect {
    rect: Rect,
    radius: i32,
}

impl RoundRect {
    /// Build the shape, clamping `radius` to something the rectangle can
    /// actually accommodate.
    ///
    /// A radius past half the shorter side has no geometric meaning -- the two
    /// corner arcs on that side would overlap and the sqrt below would go
    /// imaginary -- so it saturates at "fully round", which is also the
    /// intuitive result for a caller that passes a large radius meaning
    /// "as round as possible" (a pill).
    #[must_use]
    pub fn new(rect: Rect, radius: i32) -> Self {
        let limit = rect.w.min(rect.h) / 2;
        Self {
            rect,
            radius: radius.clamp(0, limit.max(0)),
        }
    }

    /// The rectangle being rounded.
    #[must_use]
    pub const fn rect(&self) -> Rect {
        self.rect
    }

    /// The radius actually in use, after clamping.
    #[must_use]
    pub const fn radius(&self) -> i32 {
        self.radius
    }

    /// Whether the shape encloses no pixels at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rect.w <= 0 || self.rect.h <= 0
    }

    /// How far row `y` is inset horizontally by the corner arcs.
    ///
    /// Measured at the row's *centre* (`y + 0.5`) rather than its top edge:
    /// sampling at the edge biases every corner half a pixel upward and makes
    /// the top and bottom arcs visibly asymmetric.
    #[must_use]
    pub fn inset_at(&self, y: i32) -> f32 {
        if self.radius <= 0 || self.is_empty() {
            return 0.0;
        }
        let r = f32::from(i16::try_from(self.radius).unwrap_or(i16::MAX));
        let centre = f32::from(i16::try_from(y).unwrap_or(i16::MAX)) + 0.5;
        let top = self.rect.y + self.radius;
        let bottom = self.rect.bottom() - self.radius;
        // Distance from the row to the nearest corner-arc centre line, zero
        // for every row in the straight middle section.
        let dy = if centre < f32::from(i16::try_from(top).unwrap_or(i16::MAX)) {
            f32::from(i16::try_from(top).unwrap_or(i16::MAX)) - centre
        } else if centre > f32::from(i16::try_from(bottom).unwrap_or(i16::MAX)) {
            centre - f32::from(i16::try_from(bottom).unwrap_or(i16::MAX))
        } else {
            return 0.0;
        };
        // Guard the sqrt: dy can exceed r by a fraction on the very first and
        // last row, where the arc has already closed.
        let dy = dy.min(r);
        r - r.mul_add(r, -(dy * dy)).max(0.0).sqrt()
    }

    /// The span to paint for row `y`, or `None` when the row lies outside the
    /// shape or the arcs have closed it to nothing.
    #[must_use]
    pub fn span_at(&self, y: i32) -> Option<Span> {
        if self.is_empty() || y < self.rect.y || y >= self.rect.bottom() {
            return None;
        }
        let inset = self.inset_at(y);
        let left = f32::from(i16::try_from(self.rect.x).unwrap_or(i16::MAX)) + inset;
        let right = f32::from(i16::try_from(self.rect.right()).unwrap_or(i16::MAX)) - inset;
        if right <= left {
            return None;
        }
        // `x0` is the first wholly covered pixel and `x1` the first pixel past
        // the run, so the two fractional pixels sit just outside `x0..x1`.
        #[allow(clippy::cast_possible_truncation)]
        let (x0, x1) = (left.ceil() as i32, right.floor() as i32);
        let (mut left_alpha, mut right_alpha) = (
            Self::to_alpha(left.ceil() - left),
            Self::to_alpha(right - right.floor()),
        );
        // A run that closed to nothing between two partial pixels would paint
        // its boundary twice over the same pixel; drop the coverage rather
        // than double-blending it into something too dark.
        if x1 < x0 {
            left_alpha = 0;
            right_alpha = 0;
        }
        Some(Span {
            y,
            x0,
            x1: x1.max(x0),
            left_alpha,
            right_alpha,
        })
    }

    /// Quantise a 0.0..=1.0 coverage fraction to 8-bit alpha.
    fn to_alpha(fraction: f32) -> u8 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let a = (fraction.clamp(0.0, 1.0) * 255.0).round() as u8;
        a
    }
}

#[cfg(test)]
mod tests {
    use super::RoundRect;
    use crate::draw::Rect;

    /// A zero radius must degenerate to exactly the plain rectangle, so one
    /// primitive can serve both cases without the caller branching.
    #[test]
    fn a_zero_radius_is_a_plain_rectangle() {
        let shape = RoundRect::new(Rect::new(10, 20, 40, 30), 0);
        for y in 20..50 {
            let span = shape.span_at(y).expect("row is inside the shape");
            assert_eq!((span.x0, span.x1), (10, 50), "row {y} is not square");
            assert_eq!((span.left_alpha, span.right_alpha), (0, 0));
        }
    }

    /// The straight middle of a rounded rectangle is still square: only the
    /// rows within a radius of either end are inset.
    #[test]
    fn the_middle_rows_are_not_inset() {
        let shape = RoundRect::new(Rect::new(0, 0, 100, 40), 6);
        for y in 6..34 {
            let span = shape.span_at(y).expect("row is inside the shape");
            assert_eq!((span.x0, span.x1), (0, 100), "row {y} was inset");
        }
    }

    /// The corners must actually be cut in, and progressively so -- the first
    /// row inset more than the second, and so on.
    #[test]
    fn the_corner_rows_narrow_towards_the_edge() {
        let shape = RoundRect::new(Rect::new(0, 0, 100, 40), 8);
        let mut previous = i32::MAX;
        for y in 0..8 {
            let span = shape.span_at(y).expect("row is inside the shape");
            assert!(span.x0 > 0, "row {y} should be inset");
            assert!(
                span.x0 <= previous,
                "row {y} is inset more than the row above it"
            );
            previous = span.x0;
        }
    }

    /// Top and bottom must be mirror images; sampling the arc at the wrong
    /// point in the pixel is exactly what breaks this.
    #[test]
    fn the_shape_is_symmetric_top_to_bottom() {
        let shape = RoundRect::new(Rect::new(0, 0, 60, 30), 7);
        for y in 0..15 {
            let top = shape.span_at(y).expect("top row");
            let bottom = shape.span_at(29 - y).expect("bottom row");
            assert_eq!(top.x0, bottom.x0, "row {y} is asymmetric");
            assert_eq!(top.x1, bottom.x1, "row {y} is asymmetric");
            assert_eq!(top.left_alpha, bottom.left_alpha);
        }
    }

    /// And left to right, which is what keeps a centred label looking centred.
    #[test]
    fn the_shape_is_symmetric_left_to_right() {
        let rect = Rect::new(0, 0, 60, 30);
        let shape = RoundRect::new(rect, 7);
        for y in 0..30 {
            let span = shape.span_at(y).expect("row is inside the shape");
            assert_eq!(
                span.x0 - rect.x,
                rect.right() - span.x1,
                "row {y} is not horizontally centred"
            );
            assert_eq!(span.left_alpha, span.right_alpha);
        }
    }

    /// No span may ever escape the rectangle it was asked for -- a fill that
    /// bled one pixel left would smear over the neighbouring widget.
    #[test]
    fn no_span_escapes_the_rectangle() {
        for radius in [0, 1, 2, 4, 8, 15, 40] {
            let rect = Rect::new(5, 7, 50, 30);
            let shape = RoundRect::new(rect, radius);
            for y in rect.y..rect.bottom() {
                if let Some(span) = shape.span_at(y) {
                    assert!(span.x0 >= rect.x, "radius {radius} row {y} ran left");
                    assert!(span.x1 <= rect.right(), "radius {radius} row {y} ran right");
                    assert!(span.x1 >= span.x0, "radius {radius} row {y} inverted");
                }
            }
        }
    }

    /// Rows outside the rectangle produce nothing rather than a clamped span
    /// at the edge, which would paint a stray line above the widget.
    #[test]
    fn rows_outside_the_rectangle_yield_nothing() {
        let shape = RoundRect::new(Rect::new(0, 10, 40, 20), 4);
        assert!(shape.span_at(9).is_none());
        assert!(shape.span_at(30).is_none());
        assert!(shape.span_at(-100).is_none());
        assert!(shape.span_at(10).is_some());
        assert!(shape.span_at(29).is_some());
    }

    /// An oversized radius saturates into a pill rather than producing
    /// imaginary arcs or inverted spans.
    #[test]
    fn an_oversized_radius_clamps_to_a_pill() {
        let shape = RoundRect::new(Rect::new(0, 0, 40, 20), 900);
        assert_eq!(shape.radius(), 10, "radius should clamp to half the height");
        // The widest row is the middle one. It is inset by a *fraction* of a
        // pixel -- no row centre lands exactly on the arc's widest point -- so
        // the full-coverage run stops one pixel short at either end and the
        // remainder is reported as near-total boundary coverage. Asserting
        // (0, 40) here would be demanding that the anti-aliasing round away.
        let middle = shape.span_at(10).expect("middle row");
        assert_eq!((middle.x0, middle.x1), (1, 39));
        assert!(
            middle.left_alpha > 240 && middle.right_alpha > 240,
            "the widest row's edge pixels should be nearly solid, got \
             {}/{}",
            middle.left_alpha,
            middle.right_alpha
        );
    }

    /// Degenerate rectangles must not panic or produce spans; a widget
    /// squeezed to nothing simply draws nothing.
    #[test]
    fn a_degenerate_rectangle_yields_nothing() {
        for rect in [
            Rect::new(0, 0, 0, 20),
            Rect::new(0, 0, 20, 0),
            Rect::new(0, 0, -5, -5),
        ] {
            let shape = RoundRect::new(rect, 4);
            assert!(shape.is_empty());
            assert_eq!(shape.radius(), 0);
            for y in -2..22 {
                assert!(shape.span_at(y).is_none(), "{rect:?} produced a span");
            }
        }
    }

    /// A 1px-tall or 1px-wide rectangle has no room to round, and must come
    /// out as the plain line it is.
    #[test]
    fn a_hairline_rectangle_stays_a_line() {
        let shape = RoundRect::new(Rect::new(0, 0, 100, 1), 4);
        assert_eq!(shape.radius(), 0);
        let span = shape.span_at(0).expect("the single row");
        assert_eq!((span.x0, span.x1), (0, 100));
    }

    /// The arc must be a circle, not a diagonal: at 45 degrees through the
    /// corner the inset should be r*(1 - cos45) ~= 0.293r, which a chamfer
    /// would report as ~0.5r.
    #[test]
    fn the_corner_follows_a_circle_not_a_chamfer() {
        let r = 20;
        let shape = RoundRect::new(Rect::new(0, 0, 200, 100), r);
        // The row whose centre sits r/sqrt(2) above the arc centre.
        let dy = f32::from(i16::try_from(r).unwrap_or(0)) / 2.0_f32.sqrt();
        #[allow(clippy::cast_possible_truncation)]
        let y = (f32::from(i16::try_from(r).unwrap_or(0)) - dy).round() as i32;
        let inset = shape.inset_at(y);
        let expected = f32::from(i16::try_from(r).unwrap_or(0)) * (1.0 - 1.0 / 2.0_f32.sqrt());
        assert!(
            (inset - expected).abs() < 1.0,
            "inset {inset} is not circular (expected ~{expected})"
        );
    }
}
