//! [`WeightedSpiral`] -- carves a rectangle into cells whose areas follow the
//! given weights, largest first, in strips that wind around the field.
//!
//! A strip takes as many consecutive weights as keep its cells closest to
//! square (the squarified-treemap rule), then the next strip starts on the
//! next edge. The last cell is exactly the leftover, so the cells tile the
//! area with no gap.

use platform::draw::Rect;

use crate::tile_minimum::TileMinimum;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedSpiral {
    area: Rect,
    aspect: f64,
}

impl WeightedSpiral {
    const MIN_WEIGHT: f64 = 1e-9;

    #[must_use]
    pub fn new(area: Rect) -> Self {
        Self {
            area: Rect::new(area.x, area.y, area.w.max(0), area.h.max(0)),
            aspect: 1.0,
        }
    }

    /// Aim for cells `aspect` times as wide as they are tall.
    #[must_use]
    pub fn shaped(self, aspect: f64) -> Self {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        Self { aspect, ..self }
    }

    /// One cell per weight, in order. Weights should fall as they go.
    #[must_use]
    pub fn cells(&self, weights: &[f64]) -> Vec<Rect> {
        let tall = self.stretched_height();
        if tall == self.area.h {
            return Self::square_cells(weights, self.area);
        }
        let virtual_area = Rect::new(self.area.x, 0, self.area.w, tall);
        Self::square_cells(weights, virtual_area)
            .into_iter()
            .map(|cell| {
                let top = self.unstretch(cell.y, tall);
                let bottom = self.unstretch(cell.bottom(), tall);
                Rect::new(cell.x, top, cell.w, bottom - top)
            })
            .collect()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn stretched_height(&self) -> i32 {
        (f64::from(self.area.h) * self.aspect).round() as i32
    }

    #[allow(clippy::cast_possible_truncation)]
    fn unstretch(&self, y: i32, tall: i32) -> i32 {
        if tall <= 0 {
            return self.area.y;
        }
        let scaled = (f64::from(y) * f64::from(self.area.h) / f64::from(tall)).round() as i32;
        self.area.y + scaled
    }

    fn square_cells(weights: &[f64], area: Rect) -> Vec<Rect> {
        let weights: Vec<f64> = weights
            .iter()
            .map(|&w| {
                if w.is_finite() {
                    w.max(Self::MIN_WEIGHT)
                } else {
                    Self::MIN_WEIGHT
                }
            })
            .collect();
        let mut cells = Vec::with_capacity(weights.len());
        let mut rest = area;
        let mut next = 0;
        let mut turn = 0;
        while next < weights.len() {
            let remaining = &weights[next..];
            let take = Self::strip_len(rest, remaining);
            let last = take == remaining.len();
            let share = remaining[..take].iter().sum::<f64>() / remaining.iter().sum::<f64>();
            let (strip, after) = Self::lay_strip(rest, &remaining[..take], share, turn, last);
            cells.extend(strip);
            rest = after;
            next += take;
            turn += 1;
        }
        cells
    }

    /// Whether every cell for `weights` is at least `minimum`.
    #[must_use]
    pub fn legible(&self, weights: &[f64], minimum: TileMinimum) -> bool {
        self.cells(weights).iter().all(|c| minimum.admits(c.w, c.h))
    }

    fn strip_len(rest: Rect, weights: &[f64]) -> usize {
        let short = f64::from(rest.w.min(rest.h));
        let total: f64 = weights.iter().sum();
        if short <= 0.0 || total <= 0.0 {
            return weights.len();
        }
        let scale = f64::from(rest.w) * f64::from(rest.h) / total;
        let mut sum = weights[0] * scale;
        let mut worst = Self::worst_ratio(sum, weights[0] * scale, weights[0] * scale, short);
        let mut taken = 1;
        for weight in &weights[1..] {
            let area = weight * scale;
            let grown = sum + area;
            let ratio = Self::worst_ratio(grown, weights[0] * scale, area, short);
            if ratio > worst {
                break;
            }
            sum = grown;
            worst = ratio;
            taken += 1;
        }
        taken
    }

    fn worst_ratio(sum: f64, largest: f64, smallest: f64, short: f64) -> f64 {
        let side = short.powi(2);
        let strip = sum.powi(2);
        (side * largest / strip).max(strip / (side * smallest))
    }

    fn lay_strip(
        rest: Rect,
        weights: &[f64],
        share: f64,
        turn: usize,
        last: bool,
    ) -> (Vec<Rect>, Rect) {
        let leading = turn % 4 < 2;
        let upright = rest.w >= rest.h;
        let (length, depth) = if upright {
            (rest.h, rest.w)
        } else {
            (rest.w, rest.h)
        };
        let thickness = if last {
            depth
        } else {
            Self::part(depth, share).min(depth)
        };
        let offset = if leading { 0 } else { depth - thickness };
        let total: f64 = weights.iter().sum();
        let mut before = 0.0;
        let mut start = 0;
        let mut strip = Vec::with_capacity(weights.len());
        for (index, weight) in weights.iter().enumerate() {
            before += weight;
            let end = if index + 1 == weights.len() {
                length
            } else {
                Self::part(length, before / total)
            };
            strip.push(if upright {
                Rect::new(rest.x + offset, rest.y + start, thickness, end - start)
            } else {
                Rect::new(rest.x + start, rest.y + offset, end - start, thickness)
            });
            start = end;
        }
        let kept = depth - thickness;
        let after = match (upright, leading) {
            (true, true) => Rect::new(rest.x + thickness, rest.y, kept, rest.h),
            (true, false) => Rect::new(rest.x, rest.y, kept, rest.h),
            (false, true) => Rect::new(rest.x, rest.y + thickness, rest.w, kept),
            (false, false) => Rect::new(rest.x, rest.y, rest.w, kept),
        };
        (strip, after)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn part(length: i32, share: f64) -> i32 {
        (f64::from(length.max(0)) * share).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::WeightedSpiral;
    use crate::tile_minimum::TileMinimum;
    use platform::draw::Rect;

    fn area_of(cell: Rect) -> i64 {
        i64::from(cell.w) * i64::from(cell.h)
    }

    fn falling(n: usize, decay: f64) -> Vec<f64> {
        (0..n)
            .map(|i| decay.powi(i32::try_from(i).unwrap_or(0)))
            .collect()
    }

    #[test]
    fn the_cells_tile_the_area_exactly() {
        for (w, h) in [(1280, 790), (700, 1200), (333, 97), (1, 1)] {
            let area = Rect::new(17, 23, w, h);
            for n in 1..=24 {
                let cells = WeightedSpiral::new(area).cells(&falling(n, 0.8));
                assert_eq!(cells.len(), n);
                let total: i64 = cells.iter().copied().map(area_of).sum();
                assert_eq!(total, i64::from(w) * i64::from(h), "{w}x{h} n={n}");
                for (i, a) in cells.iter().enumerate() {
                    assert!(a.x >= area.x && a.right() <= area.right());
                    assert!(a.y >= area.y && a.bottom() <= area.bottom());
                    for b in &cells[i + 1..] {
                        let across = a.right().min(b.right()) - a.x.max(b.x);
                        let down = a.bottom().min(b.bottom()) - a.y.max(b.y);
                        assert!(across <= 0 || down <= 0, "{a:?} overlaps {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn heavier_weights_get_larger_cells() {
        let cells = WeightedSpiral::new(Rect::new(0, 0, 1200, 800)).cells(&[8.0, 4.0, 2.0, 1.0]);
        let areas: Vec<i64> = cells.iter().copied().map(area_of).collect();
        assert!(areas.windows(2).all(|pair| pair[0] >= pair[1]), "{areas:?}");
        let whole: i64 = areas.iter().sum();
        assert!((areas[0] * 15 - whole * 8).abs() < whole / 20, "{areas:?}");
    }

    #[test]
    fn a_single_weight_takes_the_whole_area() {
        let area = Rect::new(5, 5, 300, 200);
        assert_eq!(WeightedSpiral::new(area).cells(&[3.0]), [area]);
    }

    #[test]
    fn no_weights_yield_no_cells() {
        assert!(
            WeightedSpiral::new(Rect::new(0, 0, 10, 10))
                .cells(&[])
                .is_empty()
        );
    }

    #[test]
    fn bad_weights_are_treated_as_tiny_not_fatal() {
        let area = Rect::new(0, 0, 400, 300);
        let cells = WeightedSpiral::new(area).cells(&[4.0, f64::NAN, -1.0, 0.0]);
        assert_eq!(cells.len(), 4);
        let total: i64 = cells.iter().copied().map(area_of).sum();
        assert_eq!(total, 400 * 300);
    }

    #[test]
    fn legibility_stops_at_the_smallest_side() {
        let spiral = WeightedSpiral::new(Rect::new(0, 0, 1000, 700));
        assert!(spiral.legible(&falling(3, 0.7), TileMinimum::square(100)));
        assert!(!spiral.legible(&falling(24, 0.5), TileMinimum::square(100)));
    }

    #[test]
    fn shaped_cells_are_wider_and_still_tile_the_area_exactly() {
        let area = Rect::new(18, 200, 1300, 640);
        let weights = falling(40, 0.98);
        let square = WeightedSpiral::new(area).cells(&weights);
        let wide = WeightedSpiral::new(area).shaped(1.5).cells(&weights);
        let total: i64 = wide.iter().copied().map(area_of).sum();
        assert_eq!(total, i64::from(area.w) * i64::from(area.h));
        assert!(
            wide.iter()
                .all(|c| c.y >= area.y && c.bottom() <= area.bottom())
        );
        let mean_aspect = |cells: &[Rect]| {
            cells
                .iter()
                .map(|c| f64::from(c.w) / f64::from(c.h))
                .sum::<f64>()
                / f64::from(u32::try_from(cells.len()).unwrap_or(1))
        };
        assert!(mean_aspect(&wide) > mean_aspect(&square) * 1.3);
    }

    #[test]
    fn a_degenerate_area_never_makes_negative_extents() {
        for area in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, -5, 40)] {
            for c in WeightedSpiral::new(area).cells(&falling(8, 0.8)) {
                assert!(c.w >= 0 && c.h >= 0, "{c:?}");
            }
        }
    }
}
