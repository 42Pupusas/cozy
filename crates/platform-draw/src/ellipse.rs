//! Ellipse outline rasterization by midpoint scan-conversion, with bounds
//! tracking for arrowheads.
//!
//! Split out of `draw.rs` for the same reason as `line.rs`: the algorithm
//! is self-contained pixel math over a bounding box.

/// A point-plotting callback, packed and clipped by the caller.
pub type Plot<'a> = &'a mut dyn FnMut(i32, i32);

/// Plot the outline of an ellipse centered at `(cx, cy)` with radii
/// `rx`, `ry`, using the integer midpoint algorithm.
///
/// Returns the horizontal extents of the center row, for anchoring an
/// arrowhead where the curve is widest.
///
/// A zero radius on either axis degenerates to a line segment along the
/// other axis (or a single point), which is what a user expects a squashed
/// ellipse to look like.
///
/// The decision variables are the textbook midpoint form scaled by 4 so the
/// `+0.25` bias of region 1 stays in integers; every increment is a multiple
/// of 4, so the comparison directions are unchanged.
pub fn outline(cx: i32, cy: i32, rx: i32, ry: i32, plot: Plot<'_>) -> (i32, i32) {
    let rx = rx.max(0);
    let ry = ry.max(0);
    if rx == 0 || ry == 0 {
        let w = rx.max(1);
        crate::line::walk(cx - w, cy, cx + w, cy, plot);
        return (cx - w, cx + w);
    }
    let mut x = 0i64;
    let mut y = i64::from(ry);
    let a2 = i64::from(rx) * i64::from(rx);
    let b2 = i64::from(ry) * i64::from(ry);
    // Region 1: |slope| <= 1, stepping x. `dx`/`dy` are 4 * d/dx, 4 * d/dy.
    let mut dx = 0i64;
    let mut dy = 8 * a2 * y;
    let mut d1 = 4 * b2 - 4 * a2 * y + a2;
    while dx < dy {
        plot_i64(cx, cy, x, y, plot);
        x += 1;
        dx += 8 * b2;
        if d1 < 0 {
            d1 += dx + 4 * b2;
        } else {
            y -= 1;
            dy -= 8 * a2;
            d1 += dx - dy + 4 * b2;
        }
    }
    // Region 2: |slope| > 1, stepping y.
    let mut d2 = b2 * (2 * x + 1) * (2 * x + 1) + 4 * a2 * (y - 1) * (y - 1) - 4 * a2 * b2;
    while y >= 0 {
        plot_i64(cx, cy, x, y, plot);
        y -= 1;
        if d2 > 0 {
            dy -= 8 * a2;
            d2 += 4 * a2 - dy;
        } else {
            x += 1;
            dx += 8 * b2;
            dy -= 8 * a2;
            d2 += dx - dy + 4 * a2;
        }
    }
    (cx - rx, cx + rx)
}

/// Plot the four symmetric points of one octant.
fn plot_i64(cx: i32, cy: i32, x: i64, y: i64, plot: Plot<'_>) {
    // Ellipse radii are window-scale (at most a few thousand), so the i64
    // walk coordinates always fit i32; saturate rather than trust it.
    let x = i32::try_from(x).unwrap_or(i32::MAX);
    let y = i32::try_from(y).unwrap_or(i32::MAX);
    plot(cx + x, cy + y);
    plot(cx - x, cy + y);
    plot(cx + x, cy - y);
    plot(cx - x, cy - y);
}

/// The point where a ray from the center at angle `theta` exits the
/// ellipse's outline.
///
/// `theta` is radians in **screen space** (y grows downward, so `pi/2`
/// points south). Used to attach an arrowhead to the ellipse's edge
/// rather than its center.
#[must_use]
pub fn edge_point(cx: i32, cy: i32, rx: i32, ry: i32, theta: f64) -> (i32, i32) {
    if rx <= 0 || ry <= 0 {
        return (cx, cy);
    }
    let cos_t = theta.cos();
    let sin_t = theta.sin();
    let denom = (cos_t * cos_t / f64::from(rx * rx) + sin_t * sin_t / f64::from(ry * ry)).sqrt();
    if denom == 0.0 {
        return (cx, cy);
    }
    // Float source, no lossless-widening path available -- a point on the
    // ellipse's edge is always near its bounding box, which is at most a
    // few thousand pixels from the center.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let x = cx + (cos_t / denom).round() as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let y = cy + (sin_t / denom).round() as i32;
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outline_pixels(cx: i32, cy: i32, rx: i32, ry: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        outline(cx, cy, rx, ry, &mut |x, y| out.push((x, y)));
        out.sort_unstable();
        out.dedup();
        out
    }

    #[test]
    fn a_circle_passes_through_the_four_cardinal_points() {
        let px = outline_pixels(10, 10, 5, 5);
        assert!(px.contains(&(5, 10)), "west");
        assert!(px.contains(&(15, 10)), "east");
        assert!(px.contains(&(10, 5)), "north");
        assert!(px.contains(&(10, 15)), "south");
    }

    #[test]
    fn every_pixel_is_within_the_bounding_box() {
        let px = outline_pixels(50, 50, 20, 10);
        for &(x, y) in &px {
            assert!((30..=70).contains(&x), "x={x} outside");
            assert!((40..=60).contains(&y), "y={y} outside");
        }
    }

    #[test]
    fn every_pixel_is_roughly_on_the_curve() {
        // ((x-cx)/rx)^2 + ((y-cy)/ry)^2 must be near 1 for outline pixels.
        let px = outline_pixels(40, 40, 12, 8);
        for &(x, y) in &px {
            let nx = f64::from(x - 40) / 12.0;
            let ny = f64::from(y - 40) / 8.0;
            let r2 = nx * nx + ny * ny;
            assert!(
                (0.65..=1.35).contains(&r2),
                "pixel ({x},{y}) at normalized radius {r2}"
            );
        }
    }

    #[test]
    fn a_zero_ry_degenerates_to_a_horizontal_segment() {
        let px = outline_pixels(8, 8, 3, 0);
        assert_eq!(
            px,
            vec![(5, 8), (6, 8), (7, 8), (8, 8), (9, 8), (10, 8), (11, 8)]
        );
    }

    #[test]
    fn the_returned_extents_span_the_full_width() {
        let (x0, x1) = outline(20, 20, 6, 4, &mut |_, _| {});
        assert_eq!(x0, 14);
        assert_eq!(x1, 26);
    }

    #[test]
    fn edge_point_lands_on_the_cardinal_directions() {
        // Screen space: theta=0 is east, pi/2 is south (y grows downward).
        assert_eq!(edge_point(10, 10, 4, 4, 0.0), (14, 10));
        let (x, y) = edge_point(10, 10, 4, 8, std::f64::consts::FRAC_PI_2);
        assert_eq!((x, y), (10, 18));
    }

    #[test]
    fn edge_point_handles_degenerate_radii() {
        assert_eq!(edge_point(5, 5, 0, 0, 1.0), (5, 5));
    }
}
