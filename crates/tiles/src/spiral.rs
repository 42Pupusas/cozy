//! [`SpiralLayout`] -- carves a rectangle into ranked cells, largest first,
//! in strips that wind around the field.
//!
//! Each cell's area is [`SpiralLayout::DECAY_PERMILLE`]/1000 of its
//! predecessor's, so sizes fall off geometrically like a golden spiral's but
//! far more slowly. A strip takes as many consecutive ranks as keep its
//! cells closest to square (the squarified-treemap rule), then the next strip
//! starts on the next edge, so the lead cell does not take most of the field
//! and no cell is a long sliver. The last cell is exactly the leftover.

use platform::draw::Rect;

use crate::squarify::Squarify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpiralLayout {
    area: Rect,
    min_side: i32,
}

impl SpiralLayout {
    /// Beyond this many cells the smallest is far under a pixel of any real
    /// window, so no screen could show it legibly.
    pub const MAX_CELLS: usize = 40;

    /// How much of its predecessor's area each cell keeps, in thousandths.
    /// The golden spiral keeps 618.
    const DECAY_PERMILLE: i32 = 875;

    #[must_use]
    pub fn new(area: Rect, min_side: i32) -> Self {
        Self {
            area: Rect::new(area.x, area.y, area.w.max(0), area.h.max(0)),
            min_side: min_side.max(1),
        }
    }

    /// The `n` cells, largest first, tiling the area exactly.
    #[must_use]
    pub fn cells(&self, n: usize) -> Vec<Rect> {
        let weights = Self::decayed(n.min(Self::MAX_CELLS));
        let mut cells = Vec::with_capacity(weights.len());
        let mut rest = self.area;
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

    /// `n` weights, each [`Self::DECAY_PERMILLE`]/1000 of the one before.
    #[must_use]
    pub fn decayed(n: usize) -> Vec<f64> {
        let decay = f64::from(Self::DECAY_PERMILLE) / 1000.0;
        let mut weight = 1.0;
        (0..n)
            .map(|_| {
                let current = weight;
                weight *= decay;
                current
            })
            .collect()
    }

    fn strip_len(rest: Rect, weights: &[f64]) -> usize {
        let short = f64::from(rest.w.min(rest.h));
        let total: f64 = weights.iter().sum();
        let scale = f64::from(rest.w) * f64::from(rest.h) / total;
        Squarify::new(short, scale).run(weights)
    }

    /// Lays `weights` as one strip along an edge of `rest`, returning its
    /// cells and the rectangle left over. `share` is the strip's fraction of
    /// `rest`'s area; the final strip takes everything that is left.
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

    /// The most cells, up to `n`, whose smallest still has both sides at
    /// least `min_side`. Never less than one for a non-empty request, so a
    /// tiny window still shows something to click.
    #[must_use]
    pub fn capacity(&self, n: usize) -> usize {
        let mut k = n.min(Self::MAX_CELLS);
        while k > 1 && !self.legible(k) {
            k -= 1;
        }
        k
    }

    fn legible(&self, k: usize) -> bool {
        self.cells(k)
            .iter()
            .all(|c| c.w >= self.min_side && c.h >= self.min_side)
    }
}

#[cfg(test)]
mod tests {
    use super::SpiralLayout;
    use platform::draw::Rect;

    fn area_of(cells: &[Rect]) -> i64 {
        cells.iter().map(|c| i64::from(c.w) * i64::from(c.h)).sum()
    }

    #[test]
    fn legible_layouts_stay_close_to_square() {
        for (w, h) in [(1280, 790), (1000, 700), (1900, 1000), (700, 1100)] {
            let layout = SpiralLayout::new(Rect::new(0, 0, w, h), 152);
            for c in layout.cells(layout.capacity(24)) {
                assert!(
                    c.w.max(c.h) * 10 <= c.w.min(c.h) * 20,
                    "{w}x{h}: {c:?} is more than 2:1"
                );
            }
        }
    }

    #[test]
    fn the_lead_cell_stays_well_under_half_the_field() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 1).cells(10);
        assert!(area_of(&cells[..1]) * 5 < area_of(&cells) * 2, "{cells:?}");
    }

    #[test]
    fn the_first_cell_is_the_largest() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 1).cells(10);
        let largest = cells.iter().map(|c| area_of(&[*c])).max().expect("cells");
        assert_eq!(area_of(&cells[..1]), largest, "{cells:?}");
    }

    #[test]
    fn capacity_stops_before_cells_become_illegible() {
        let layout = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 64);
        let k = layout.capacity(100);
        assert!(k > 1 && k < 100);
        let cells = layout.cells(k);
        assert!(cells.iter().all(|c| c.w >= 64 && c.h >= 64), "{cells:?}");
        let over = layout.cells(k + 1);
        assert!(over.iter().any(|c| c.w < 64 || c.h < 64));
    }

    #[test]
    fn capacity_never_exceeds_the_request() {
        let layout = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 1);
        assert_eq!(layout.capacity(3), 3);
    }

    #[test]
    fn a_window_too_small_for_any_tile_still_offers_one() {
        let layout = SpiralLayout::new(Rect::new(0, 0, 10, 10), 64);
        assert_eq!(layout.capacity(40), 1);
    }

    #[test]
    fn the_cells_tile_the_area_exactly() {
        for (w, h) in [(1280, 790), (700, 1200), (333, 97), (1, 1)] {
            let area = Rect::new(17, 23, w, h);
            for n in 1..=SpiralLayout::MAX_CELLS {
                let cells = SpiralLayout::new(area, 1).cells(n);
                assert_eq!(cells.len(), n, "{w}x{h}");
                assert_eq!(
                    area_of(&cells),
                    i64::from(w) * i64::from(h),
                    "{w}x{h} n={n}"
                );
                for (i, a) in cells.iter().enumerate() {
                    assert!(a.x >= area.x && a.right() <= area.right());
                    assert!(a.y >= area.y && a.bottom() <= area.bottom());
                    for b in &cells[i + 1..] {
                        let overlap_w = a.right().min(b.right()) - a.x.max(b.x);
                        let overlap_h = a.bottom().min(b.bottom()) - a.y.max(b.y);
                        assert!(overlap_w <= 0 || overlap_h <= 0, "{a:?} overlaps {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_weights_decay_from_one() {
        let weights = SpiralLayout::decayed(3);
        assert!((weights[0] - 1.0).abs() < f64::EPSILON);
        assert!(weights[1] < weights[0] && weights[2] < weights[1]);
    }

    #[test]
    fn requests_are_capped() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 1).cells(5000);
        assert_eq!(cells.len(), SpiralLayout::MAX_CELLS);
    }

    #[test]
    fn nothing_requested_yields_nothing() {
        let layout = SpiralLayout::new(Rect::new(0, 0, 100, 100), 1);
        assert!(layout.cells(0).is_empty());
        assert_eq!(layout.capacity(0), 0);
    }

    #[test]
    fn one_cell_takes_the_whole_area() {
        let area = Rect::new(5, 5, 300, 200);
        assert_eq!(SpiralLayout::new(area, 1).cells(1), [area]);
    }

    #[test]
    fn a_degenerate_area_never_produces_negative_extents() {
        for area in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, -5, 40)] {
            for c in SpiralLayout::new(area, 1).cells(8) {
                assert!(c.w >= 0 && c.h >= 0, "{c:?}");
            }
        }
    }
}
