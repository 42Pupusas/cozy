//! [`SourceSpiral`] -- carves the room around a fixed source rectangle into
//! ranked cells, largest first, in strips that wind in from the field's edge
//! to the source.
//!
//! Each strip runs the whole length of one side of the room still free and
//! is never deeper than the gap between that side and the source, so the
//! source is never cut. The next strip goes on the side with the widest gap
//! left. A gap narrower than the smallest legible cell stays empty.

use platform::draw::Rect;

use crate::spiral::SpiralLayout;
use crate::spiral_side::SpiralSide;
use crate::squarify::Squarify;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpiral {
    area: Rect,
    hub: Rect,
    min_side: i32,
}

impl SourceSpiral {
    #[must_use]
    pub fn new(area: Rect, source: Rect, min_side: i32) -> Self {
        let area = Rect::new(area.x, area.y, area.w.max(0), area.h.max(0));
        let min_side = min_side.max(1);
        Self {
            area,
            hub: Self::hub(area, source, min_side),
            min_side,
        }
    }

    /// `source` kept inside `area` and widened to any edge it is too close to
    /// for a legible cell to fit between them.
    fn hub(area: Rect, source: Rect, min_side: i32) -> Rect {
        let mut left = source.x.clamp(area.x, area.right());
        let mut top = source.y.clamp(area.y, area.bottom());
        let mut right = source.right().clamp(left, area.right());
        let mut bottom = source.bottom().clamp(top, area.bottom());
        if left - area.x < min_side {
            left = area.x;
        }
        if top - area.y < min_side {
            top = area.y;
        }
        if area.right() - right < min_side {
            right = area.right();
        }
        if area.bottom() - bottom < min_side {
            bottom = area.bottom();
        }
        Rect::new(left, top, right - left, bottom - top)
    }

    /// Up to `n` cells, largest first, none overlapping the source. Fewer
    /// when the room around the source runs out.
    #[must_use]
    pub fn cells(&self, n: usize) -> Vec<Rect> {
        let weights = SpiralLayout::decayed(n.min(SpiralLayout::MAX_CELLS));
        let mut cells = Vec::with_capacity(weights.len());
        let mut rest = self.area;
        let mut next = 0;
        while next < weights.len() {
            let open: Vec<SpiralSide> = SpiralSide::ALL
                .into_iter()
                .filter(|side| side.gap(rest, self.hub) > 0)
                .collect();
            let Some(side) = Self::widest(&open, rest, self.hub) else {
                break;
            };
            let remaining = &weights[next..];
            let (take, thickness) = self.strip(side, rest, remaining, open.len());
            let strip = side.strip(rest, thickness);
            cells.extend(Self::lay(side, strip, &remaining[..take]));
            rest = side.remainder(rest, thickness);
            next += take;
        }
        cells
    }

    fn widest(open: &[SpiralSide], rest: Rect, hub: Rect) -> Option<SpiralSide> {
        open.iter()
            .copied()
            .rev()
            .max_by_key(|side| side.gap(rest, hub))
    }

    /// How many of `remaining` the strip on `side` takes, and how deep it is.
    /// Enough weights are held back for every other open side to get one.
    fn strip(&self, side: SpiralSide, rest: Rect, remaining: &[f64], open: usize) -> (usize, i32) {
        let gap = side.gap(rest, self.hub);
        let length = side.length(rest);
        let room = Self::area_of(rest) - Self::area_of(self.hub);
        let total: f64 = remaining.iter().sum();
        let scale = room / total;
        let most = remaining.len().saturating_sub(open - 1).max(1);
        let mut take = Squarify::new(f64::from(length), scale).run(&remaining[..most]);
        if take == remaining.len() {
            return (take, gap);
        }
        let share: f64 = remaining[..take].iter().sum::<f64>() * scale;
        let thickness = Self::round(share / f64::from(length.max(1))).clamp(1, gap);
        if thickness > gap - self.min_side {
            if open == 1 {
                take = remaining.len();
            }
            return (take, gap);
        }
        (take, thickness)
    }

    fn lay(side: SpiralSide, strip: Rect, weights: &[f64]) -> Vec<Rect> {
        let length = side.length(strip);
        let total: f64 = weights.iter().sum();
        let mut before = 0.0;
        let mut start = 0;
        weights
            .iter()
            .enumerate()
            .map(|(index, weight)| {
                before += weight;
                let end = if index + 1 == weights.len() {
                    length
                } else {
                    Self::round(f64::from(length) * before / total)
                };
                let piece = side.piece(strip, start, end);
                start = end;
                piece
            })
            .collect()
    }

    fn area_of(rect: Rect) -> f64 {
        f64::from(rect.w.max(0)) * f64::from(rect.h.max(0))
    }

    #[allow(clippy::cast_possible_truncation)]
    const fn round(value: f64) -> i32 {
        value.round() as i32
    }

    /// The most cells, up to `n`, that all fit with both sides at least the
    /// smallest legible side. Never more than one when even one cell does
    /// not fit, and none when the source leaves no room at all.
    #[must_use]
    pub fn capacity(&self, n: usize) -> usize {
        let mut k = n.min(SpiralLayout::MAX_CELLS);
        while k > 1 && !self.legible(k) {
            k -= 1;
        }
        k.min(self.cells(k).len())
    }

    fn legible(&self, k: usize) -> bool {
        let cells = self.cells(k);
        cells.len() == k
            && cells
                .iter()
                .all(|c| c.w >= self.min_side && c.h >= self.min_side)
    }
}

#[cfg(test)]
mod tests {
    use super::SourceSpiral;
    use platform::draw::Rect;

    const AREA: Rect = Rect::new(10, 10, 1280, 790);
    const SOURCE: Rect = Rect::new(500, 400, 320, 200);

    fn apart(a: Rect, b: Rect) -> bool {
        a.right() <= b.x || b.right() <= a.x || a.bottom() <= b.y || b.bottom() <= a.y
    }

    fn area_of(cells: &[Rect]) -> i64 {
        cells.iter().map(|c| i64::from(c.w) * i64::from(c.h)).sum()
    }

    #[test]
    fn no_cell_touches_the_source_or_another_cell() {
        for n in 1..=24 {
            let cells = SourceSpiral::new(AREA, SOURCE, 80).cells(n);
            for (i, a) in cells.iter().enumerate() {
                assert!(apart(*a, SOURCE), "n={n}: {a:?} overlaps the source");
                for b in &cells[i + 1..] {
                    assert!(apart(*a, *b), "n={n}: {a:?} overlaps {b:?}");
                }
            }
        }
    }

    #[test]
    fn every_cell_stays_inside_the_area() {
        for n in 1..=24 {
            for c in SourceSpiral::new(AREA, SOURCE, 80).cells(n) {
                assert!(c.x >= AREA.x && c.right() <= AREA.right(), "{c:?}");
                assert!(c.y >= AREA.y && c.bottom() <= AREA.bottom(), "{c:?}");
            }
        }
    }

    #[test]
    fn with_enough_tiles_the_cells_fill_everything_but_the_source() {
        let spiral = SourceSpiral::new(AREA, SOURCE, 80);
        let k = spiral.capacity(40);
        assert!(k >= 6, "{k}");
        let cells = spiral.cells(k);
        assert_eq!(
            area_of(&cells) + area_of(&[SOURCE]),
            i64::from(AREA.w) * i64::from(AREA.h)
        );
    }

    #[test]
    fn the_first_cell_is_the_largest() {
        let cells = SourceSpiral::new(AREA, SOURCE, 1).cells(10);
        let largest = cells.iter().map(|c| area_of(&[*c])).max().expect("cells");
        assert_eq!(area_of(&cells[..1]), largest, "{cells:?}");
    }

    #[test]
    fn capacity_keeps_every_cell_legible() {
        for source in [
            SOURCE,
            Rect::new(10, 10, 300, 200),
            Rect::new(900, 500, 390, 300),
        ] {
            let spiral = SourceSpiral::new(AREA, source, 120);
            let k = spiral.capacity(40);
            let cells = spiral.cells(k);
            assert_eq!(cells.len(), k);
            assert!(cells.iter().all(|c| c.w >= 120 && c.h >= 120), "{cells:?}");
        }
    }

    #[test]
    fn a_source_in_a_corner_leaves_the_tiles_the_rest() {
        let corner = Rect::new(AREA.x, AREA.y, 300, 200);
        let spiral = SourceSpiral::new(AREA, corner, 80);
        let cells = spiral.cells(spiral.capacity(40));
        assert!(cells.iter().all(|c| apart(*c, corner)));
        assert_eq!(
            area_of(&cells) + area_of(&[corner]),
            i64::from(AREA.w) * i64::from(AREA.h)
        );
    }

    #[test]
    fn a_gap_too_thin_for_a_cell_stays_empty() {
        let near = Rect::new(AREA.x + 30, AREA.y + 30, 400, 300);
        let cells = SourceSpiral::new(AREA, near, 80).cells(12);
        for c in &cells {
            assert!(
                c.x >= near.right() || c.y >= near.bottom(),
                "{c:?} is in the thin gap"
            );
        }
    }

    #[test]
    fn a_source_over_the_whole_area_leaves_no_room() {
        let spiral = SourceSpiral::new(AREA, AREA, 80);
        assert!(spiral.cells(5).is_empty());
        assert_eq!(spiral.capacity(5), 0);
    }

    #[test]
    fn a_source_hanging_past_the_area_is_held_inside_it() {
        let hanging = Rect::new(AREA.right() - 100, AREA.bottom() - 100, 400, 400);
        for c in SourceSpiral::new(AREA, hanging, 80).cells(12) {
            assert!(c.right() <= AREA.right() && c.bottom() <= AREA.bottom());
        }
    }

    #[test]
    fn moving_the_source_a_little_moves_the_cells_a_little() {
        let before = SourceSpiral::new(AREA, SOURCE, 100).cells(8);
        let nudged = Rect::new(SOURCE.x + 6, SOURCE.y, SOURCE.w, SOURCE.h);
        let after = SourceSpiral::new(AREA, nudged, 100).cells(8);
        assert_eq!(before.len(), after.len());
        for (a, b) in before.iter().zip(&after) {
            let shift = (a.x - b.x).abs() + (a.right() - b.right()).abs();
            assert!(shift <= 40, "{a:?} jumped to {b:?}");
        }
    }

    #[test]
    fn nothing_requested_yields_nothing() {
        let spiral = SourceSpiral::new(AREA, SOURCE, 1);
        assert!(spiral.cells(0).is_empty());
        assert_eq!(spiral.capacity(0), 0);
    }
}
