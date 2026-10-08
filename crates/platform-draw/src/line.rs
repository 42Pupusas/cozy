//! Line rasterization: one Bresenham walk, plus thickness by perpendicular
//! offset.
//!
//! Split out of `draw.rs` because the walk algorithms are self-contained
//! pixel math, while `Canvas` stays the clipping/packing surface that calls
//! them.

/// Walk the pixels of a line from `(x0, y0)` to `(x1, y1)`, invoking `plot`
/// for each. The caller packs and clips.
///
/// Classic integer Bresenham: the error accumulator advances the minor axis
/// one step per major-axis pixel, so no floating point and no drift.
pub fn walk(x0: i32, y0: i32, x1: i32, y1: i32, mut plot: impl FnMut(i32, i32)) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let step_x = if x0 < x1 { 1 } else { -1 };
    let step_y = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        plot(x, y);
        if x == x1 && y == y1 {
            return;
        }
        let twice = 2 * error;
        if twice >= dy {
            error += dy;
            x += step_x;
        }
        if twice <= dx {
            error += dx;
            y += step_y;
        }
    }
}

/// The unit perpendicular of the line `(x0, y0) -> (x1, y1)`, rounded to
/// whole pixels.
///
/// Used to offset parallel copies of a line for thickness. The rounded
/// perpendicular of a zero-length line is zero, which degenerates the
/// thickness loop to a single copy: correct, since a point has no direction.
fn perpendicular(x0: i32, y0: i32, x1: i32, y1: i32) -> (i32, i32) {
    let dx = f64::from(x1 - x0);
    let dy = f64::from(y1 - y0);
    let length = dx.hypot(dy);
    if length < f64::EPSILON {
        return (0, 0);
    }
    // Float source, no lossless-widening path available -- a unit
    // perpendicular's components are always within [-1.0, 1.0].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let px = (-dy / length).round() as i32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let py = (dx / length).round() as i32;
    (px, py)
}

/// Plot every pixel of a line `width` pixels thick.
///
/// Thickness comes from `width` parallel 1px walks, offset along the
/// perpendicular. Centered when `width` is odd; biased toward the origin
/// side when even. `width < 1` draws nothing.
pub fn walk_thick(x0: i32, y0: i32, x1: i32, y1: i32, width: i32, mut plot: impl FnMut(i32, i32)) {
    if width < 1 {
        return;
    }
    let (px, py) = perpendicular(x0, y0, x1, y1);
    let half = (width - 1) / 2;
    let even_bias = (width - 1) % 2;
    for step in -half..=half + even_bias {
        walk(
            x0 + px * step,
            y0 + py * step,
            x1 + px * step,
            y1 + py * step,
            &mut plot,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        walk(x0, y0, x1, y1, |x, y| out.push((x, y)));
        out
    }

    #[test]
    fn a_horizontal_line_visits_every_column_exactly_once() {
        let px = pixels(2, 5, 6, 5);
        assert_eq!(px, vec![(2, 5), (3, 5), (4, 5), (5, 5), (6, 5)]);
    }

    #[test]
    fn a_vertical_line_visits_every_row_exactly_once() {
        let px = pixels(4, 1, 4, 4);
        assert_eq!(px, vec![(4, 1), (4, 2), (4, 3), (4, 4)]);
    }

    #[test]
    fn a_diagonal_line_steps_both_axes_together() {
        let px = pixels(0, 0, 3, 3);
        assert_eq!(px, vec![(0, 0), (1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn the_walk_is_symmetric_in_direction() {
        // Reversing the endpoints must produce the same pixel set: a stroke
        // must not change appearance because the user drew it backwards.
        let forward = pixels(0, 0, 7, 3);
        let backward = pixels(7, 3, 0, 0);
        let mut both = forward.clone();
        both.extend(backward);
        both.sort_unstable();
        both.dedup();
        assert_eq!(both.len(), forward.len(), "reverse walk added pixels");
    }

    #[test]
    fn a_single_point_visits_itself() {
        assert_eq!(pixels(5, 5, 5, 5), vec![(5, 5)]);
    }

    #[test]
    fn a_negative_slope_line_stays_connected() {
        // Every step must touch its neighbour in at least one axis, or the
        // line reads as a dotted path.
        let px = pixels(0, 4, 6, 0);
        for pair in px.windows(2) {
            let dx = (pair[1].0 - pair[0].0).abs();
            let dy = (pair[1].1 - pair[0].1).abs();
            assert!(dx <= 1 && dy <= 1, "gap between {pair:?}");
        }
        assert_eq!(px.first(), Some(&(0, 4)));
        assert_eq!(px.last(), Some(&(6, 0)));
    }

    #[test]
    fn a_thick_line_covers_more_pixels_than_a_thin_one() {
        let mut thin = 0;
        walk_thick(0, 0, 10, 0, 1, |_, _| thin += 1);
        let mut thick = 0;
        walk_thick(0, 0, 10, 0, 3, |_, _| thick += 1);
        assert_eq!(thin, 11);
        assert_eq!(thick, 33, "three rows of eleven pixels");
    }

    #[test]
    fn a_zero_width_line_draws_nothing() {
        let mut count = 0;
        walk_thick(0, 0, 5, 5, 0, |_, _| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn an_even_width_is_biased_not_dropped() {
        // Width 2 must draw two copies, not degenerate to one.
        let mut count = 0;
        walk_thick(0, 0, 10, 0, 2, |_, _| count += 1);
        assert_eq!(count, 22);
    }

    #[test]
    fn thickness_perpendicular_to_a_vertical_line_spreads_horizontally() {
        let mut xs = Vec::new();
        walk_thick(5, 0, 5, 2, 3, |x, y| {
            if y == 1 {
                xs.push(x);
            }
        });
        xs.sort_unstable();
        assert_eq!(xs, vec![4, 5, 6]);
    }
}
