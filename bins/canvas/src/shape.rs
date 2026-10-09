//! [`Shape`] -- one element on the canvas: a rectangle, ellipse, arrow, or
//! text note.
//!
//! Pure data plus pure geometry (hit-testing, bounds, translation). No
//! pixels, no Wayland -- every rule here is unit-testable without a
//! compositor, exactly like `widget`'s geometry types.

/// A selectable, movable element on the canvas.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Axis-aligned rectangle outline from a drag.
    Rect { x: f64, y: f64, w: f64, h: f64 },
    /// Ellipse outline inscribed in a drag rect.
    Ellipse { x: f64, y: f64, w: f64, h: f64 },
    /// Line from `a` to `b` with an arrowhead at `b`.
    Arrow { ax: f64, ay: f64, bx: f64, by: f64 },
    /// A text note: an editable card anchored at its top-left.
    Note {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        text: String,
    },
}

/// How close (in screen pixels, already divided by zoom by the caller) a
/// point must be to a line or a shape edge for a hit.
pub const HIT_SLOP: f64 = 6.0;

/// The geometry of an arrow's two barbs.
///
/// Lives here, beside the rest of the shape maths, rather than in the
/// window: it is pure trigonometry with a correctness property worth
/// testing, and the window only turns the result into pixels.
pub struct ArrowHead;

impl ArrowHead {
    /// Angle between the shaft and each barb, in radians. Just past a right
    /// angle so the barbs sweep back rather than sticking out sideways.
    pub const ANGLE: f64 = std::f64::consts::PI * 5.0 / 6.0;

    /// The two barb directions for a shaft pointing along `(ux, uy)`, which
    /// must be a unit vector.
    ///
    /// Both barbs point *back* from the tip, mirrored about the shaft. Only
    /// the sine carries the sign: `cos(-t) == cos(t)`, so negating both
    /// terms would negate the vector rather than mirror it, and the second
    /// barb would shoot forward past the tip as a continuation of the
    /// shaft. That renders as a one-sided arrowhead.
    #[must_use]
    pub fn barbs(ux: f64, uy: f64) -> [(f64, f64); 2] {
        let ca = Self::ANGLE.cos();
        [1.0, -1.0].map(|sign| {
            let sa = Self::ANGLE.sin() * sign;
            (uy.mul_add(-sa, ux * ca), uy.mul_add(ca, ux * sa))
        })
    }
}

impl Shape {
    /// The shape's bounding box in world coordinates.
    #[must_use]
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        match *self {
            Self::Rect { x, y, w, h }
            | Self::Ellipse { x, y, w, h }
            | Self::Note { x, y, w, h, .. } => (x, y, w, h),
            Self::Arrow { ax, ay, bx, by } => {
                let x = ax.min(bx);
                let y = ay.min(by);
                (x, y, (bx - ax).abs(), (by - ay).abs())
            }
        }
    }

    /// Whether a world point `p` hits this shape, where lines and shape
    /// *edges* are `slop`-wide (the caller passes `HIT_SLOP / zoom` so the
    /// slop stays a constant screen distance at any zoom).
    ///
    /// Filled areas (a rect's interior, a note's card) hit anywhere inside:
    /// a note must be trivially grabbable across its whole face, not just
    /// along its border.
    #[must_use]
    pub fn hits(&self, p: (f64, f64), slop: f64) -> bool {
        match *self {
            Self::Rect { x, y, w, h } => Self::rect_edge_hits(p, x, y, w, h, slop),
            Self::Ellipse { x, y, w, h } => {
                let rx = w / 2.0;
                let ry = h / 2.0;
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let cx = x + rx;
                let cy = y + ry;
                let nx = (p.0 - cx) / rx;
                let ny = (p.1 - cy) / ry;
                let r = nx.hypot(ny);
                // Edge band: within slop of the outline, measured in world
                // units along the radial direction -- an approximation that
                // is exact for circles and close enough for ellipses.
                let slop_r = slop / rx.min(ry);
                (r - 1.0).abs() <= slop_r
            }
            Self::Arrow { ax, ay, bx, by } => {
                Self::point_segment_distance(p, (ax, ay), (bx, by)) <= slop
            }
            Self::Note { x, y, w, h, .. } => {
                p.0 >= x - slop && p.0 <= x + w + slop && p.1 >= y - slop && p.1 <= y + h + slop
            }
        }
    }

    /// Translate the shape by a world delta.
    pub fn translate(&mut self, dx: f64, dy: f64) {
        match self {
            Self::Rect { x, y, .. } | Self::Ellipse { x, y, .. } | Self::Note { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
            Self::Arrow { ax, ay, bx, by } => {
                *ax += dx;
                *ay += dy;
                *bx += dx;
                *by += dy;
            }
        }
    }

    /// Whether a world point hits a rect's *outline* (not its interior):
    /// inside the grown rect but outside the shrunk one.
    #[allow(clippy::many_single_char_names)]
    fn rect_edge_hits(p: (f64, f64), x: f64, y: f64, w: f64, h: f64, slop: f64) -> bool {
        let inside_outer =
            p.0 >= x - slop && p.0 <= x + w + slop && p.1 >= y - slop && p.1 <= y + h + slop;
        let inside_inner =
            p.0 >= x + slop && p.0 <= x + w - slop && p.1 >= y + slop && p.1 <= y + h - slop;
        inside_outer && !inside_inner
    }

    /// Distance from point `p` to the segment `a`-`b`.
    fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
        let (px, py) = p;
        let (ax, ay) = a;
        let (bx, by) = b;
        let dx = bx - ax;
        let dy = by - ay;
        let length_sq = dy.mul_add(dy, dx * dx);
        if length_sq < f64::EPSILON {
            return (px - ax).hypot(py - ay);
        }
        let t = ((py - ay).mul_add(dy, (px - ax) * dx) / length_sq).clamp(0.0, 1.0);
        let (cx, cy) = (ax + t * dx, ay + t * dy);
        (px - cx).hypot(py - cy)
    }
}

#[cfg(test)]
mod arrow_head_tests {
    use super::ArrowHead;

    /// The diagonal unit component, so the diagonal cases stay unit length.
    const D: f64 = std::f64::consts::FRAC_1_SQRT_2;

    /// A spread of shaft directions, including the axis-aligned cases where
    /// a sign error is easiest to overlook.
    const DIRECTIONS: [(f64, f64); 8] = [
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, 1.0),
        (0.0, -1.0),
        (D, D),
        (-D, D),
        (D, -D),
        (-D, -D),
    ];

    /// How far along the shaft a barb reaches. Negative points backward.
    fn along(u: (f64, f64), w: (f64, f64)) -> f64 {
        u.0.mul_add(w.0, u.1 * w.1)
    }

    /// Which side of the shaft a barb falls on, by the 2D cross product.
    fn side(u: (f64, f64), w: (f64, f64)) -> f64 {
        u.0.mul_add(w.1, -(u.1 * w.0))
    }

    /// The bug that shipped: both barbs must lean back from the tip.
    ///
    /// The broken version negated cosine along with sine, so the second
    /// barb came out as the exact negative of the first and ran forward
    /// past the tip, continuing the shaft. On screen only one barb showed.
    #[test]
    fn both_barbs_point_back_from_the_tip() {
        for u in DIRECTIONS {
            for barb in ArrowHead::barbs(u.0, u.1) {
                assert!(
                    along(u, barb) < 0.0,
                    "a barb ran past the tip for shaft {u:?}: {barb:?}"
                );
            }
        }
    }

    #[test]
    fn the_barbs_straddle_the_shaft() {
        for u in DIRECTIONS {
            let [p, m] = ArrowHead::barbs(u.0, u.1);
            assert!(
                side(u, p) * side(u, m) < 0.0,
                "both barbs fell on one side for shaft {u:?}"
            );
        }
    }

    #[test]
    fn a_barb_is_never_the_negation_of_its_twin() {
        // The precise signature of the old bug: barbs summing to zero.
        for u in DIRECTIONS {
            let [p, m] = ArrowHead::barbs(u.0, u.1);
            let sum = (p.0 + m.0).hypot(p.1 + m.1);
            assert!(
                sum > 1e-6,
                "barbs are exact negatives for shaft {u:?}: the head is one-sided"
            );
        }
    }

    #[test]
    fn the_barbs_keep_the_unit_length_of_the_shaft() {
        for u in DIRECTIONS {
            for barb in ArrowHead::barbs(u.0, u.1) {
                let len = barb.0.hypot(barb.1);
                assert!((len - 1.0).abs() < 1e-9, "barb length drifted: {len}");
            }
        }
    }

    #[test]
    fn each_barb_sits_at_the_declared_angle_from_the_shaft() {
        for u in DIRECTIONS {
            for barb in ArrowHead::barbs(u.0, u.1) {
                // Both are unit vectors, so the dot product is the cosine.
                let cos = along(u, barb);
                assert!(
                    (cos - ArrowHead::ANGLE.cos()).abs() < 1e-9,
                    "barb angle drifted for shaft {u:?}"
                );
            }
        }
    }

    #[test]
    fn an_eastward_shaft_puts_one_barb_above_and_one_below() {
        // Screen space: y grows downward. Both barbs sit behind the tip,
        // one above the shaft and one below.
        let [p, m] = ArrowHead::barbs(1.0, 0.0);
        assert!(p.0 < 0.0 && m.0 < 0.0, "both barbs must sit behind the tip");
        assert!(
            p.1.signum() != m.1.signum(),
            "an eastward arrow needs one barb up and one down"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOP: f64 = 6.0;

    #[test]
    fn a_rect_hits_on_its_outline_not_its_interior() {
        let rect = Shape::Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        assert!(rect.hits((0.0, 25.0), SLOP), "left edge");
        assert!(rect.hits((100.0, 25.0), SLOP), "right edge");
        assert!(rect.hits((50.0, 0.0), SLOP), "top edge");
        assert!(!rect.hits((50.0, 25.0), SLOP), "dead center must miss");
    }

    #[test]
    fn an_ellipse_hits_on_its_curve() {
        let ellipse = Shape::Ellipse {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        assert!(ellipse.hits((50.0, 0.0), SLOP), "north point");
        assert!(ellipse.hits((0.0, 50.0), SLOP), "west point");
        assert!(!ellipse.hits((50.0, 50.0), SLOP), "center must miss");
        assert!(!ellipse.hits((50.0, 20.0), SLOP), "well inside must miss");
    }

    #[test]
    fn an_arrow_hits_along_its_shaft() {
        let arrow = Shape::Arrow {
            ax: 0.0,
            ay: 0.0,
            bx: 100.0,
            by: 0.0,
        };
        assert!(arrow.hits((50.0, 0.0), SLOP));
        assert!(arrow.hits((50.0, 4.0), SLOP), "within slop");
        assert!(!arrow.hits((50.0, 12.0), SLOP), "past slop");
        // Past either endpoint extended: a miss.
        assert!(!arrow.hits((-20.0, 0.0), SLOP));
        assert!(!arrow.hits((120.0, 0.0), SLOP));
    }

    #[test]
    fn a_note_hits_across_its_whole_face() {
        let note = Shape::Note {
            x: 10.0,
            y: 10.0,
            w: 80.0,
            h: 40.0,
            text: "hello".to_owned(),
        };
        assert!(note.hits((50.0, 30.0), SLOP), "center of the card");
        assert!(note.hits((10.0, 10.0), SLOP), "corner");
        assert!(!note.hits((0.0, 0.0), SLOP), "outside");
    }

    #[test]
    fn a_degenerate_arrow_hits_at_its_point() {
        // A click, not a drag: zero-length arrow. It must still be
        // selectable at its own location.
        let arrow = Shape::Arrow {
            ax: 40.0,
            ay: 40.0,
            bx: 40.0,
            by: 40.0,
        };
        assert!(arrow.hits((42.0, 41.0), SLOP));
        assert!(!arrow.hits((60.0, 40.0), SLOP));
    }

    #[test]
    fn a_very_thin_rect_still_hits_near_its_center() {
        // A 2px-tall rect dragged horizontally: both edges' slop bands
        // overlap, so the whole line is grabbable.
        let rect = Shape::Rect {
            x: 0.0,
            y: 50.0,
            w: 100.0,
            h: 2.0,
        };
        assert!(rect.hits((50.0, 51.0), SLOP));
        assert!(!rect.hits((50.0, 70.0), SLOP));
    }

    #[test]
    fn translation_moves_every_kind_of_shape() {
        let mut rect = Shape::Rect {
            x: 1.0,
            y: 2.0,
            w: 10.0,
            h: 10.0,
        };
        rect.translate(5.0, -3.0);
        assert_eq!(
            rect,
            Shape::Rect {
                x: 6.0,
                y: -1.0,
                w: 10.0,
                h: 10.0
            }
        );

        let mut arrow = Shape::Arrow {
            ax: 0.0,
            ay: 0.0,
            bx: 10.0,
            by: 10.0,
        };
        arrow.translate(1.0, 1.0);
        assert_eq!(
            arrow,
            Shape::Arrow {
                ax: 1.0,
                ay: 1.0,
                bx: 11.0,
                by: 11.0
            }
        );
    }

    #[test]
    fn bounds_cover_each_shape() {
        let arrow = Shape::Arrow {
            ax: 10.0,
            ay: 30.0,
            bx: -20.0,
            by: 5.0,
        };
        assert_eq!(arrow.bounds(), (-20.0, 5.0, 30.0, 25.0));
        let note = Shape::Note {
            x: 5.0,
            y: 6.0,
            w: 70.0,
            h: 30.0,
            text: String::new(),
        };
        assert_eq!(note.bounds(), (5.0, 6.0, 70.0, 30.0));
    }

    #[test]
    fn the_distance_to_a_segment_matches_the_geometry() {
        let d = Shape::point_segment_distance((0.0, 5.0), (0.0, 0.0), (10.0, 0.0));
        assert!((d - 5.0).abs() < 1e-9);
        // Nearest point clamped to the endpoint.
        let d = Shape::point_segment_distance((20.0, 0.0), (0.0, 0.0), (10.0, 0.0));
        assert!((d - 10.0).abs() < 1e-9);
        // Perpendicular foot inside the segment.
        let d = Shape::point_segment_distance((5.0, 3.0), (0.0, 0.0), (10.0, 0.0));
        assert!((d - 3.0).abs() < 1e-9);
    }
}
