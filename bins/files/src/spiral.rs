//! [`SpiralLayout`] -- carves a rectangle into ranked cells along a spiral,
//! largest first.
//!
//! Every cell takes [`SpiralLayout::CUT`]/[`SpiralLayout::CUT_DEN`] of the
//! remaining long side, winding inward. That fixed share is the whole point:
//! the lead tile keeps a visible lead (it holds 2/5 of the field's area, the
//! runner-up 6/25) while each later cell is only 3/5 the size of its
//! predecessor, so the tail stays legible instead of starving the way the
//! golden cut's F(m)/F(m+1) ratio did -- it converges to 0.618, handed the
//! first cell ~62% of the field and left everything past rank three under a
//! fifth of it.

use platform::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MinCell {
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpiralLayout {
    area: Rect,
    min_side: i32,
}

impl SpiralLayout {
    /// Beyond this many cells the smallest is far under a pixel of any real
    /// window, so no screen could show it legibly.
    pub const MAX_CELLS: usize = 24;

    /// The share of the remaining long side each cut takes, over
    /// [`SpiralLayout::CUT_DEN`]. Below 1/2 the cells stay card-shaped and
    /// the wind keeps its alternation; the old golden ratio (5/8) is the
    /// steepest decay worth calling a spiral.
    const CUT: i64 = 2;
    const CUT_DEN: i64 = 5;

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
        let n = n.min(Self::MAX_CELLS);
        let mut cells = Vec::with_capacity(n);
        let mut rest = self.area;
        for k in 0..n {
            let remaining = n - k;
            if remaining == 1 {
                cells.push(rest);
                break;
            }
            let (cell, next) = Self::cut(rest, k);
            cells.push(cell);
            rest = next;
        }
        cells
    }

    /// The most cells, up to `n`, whose smallest still has both sides at
    /// least `min_side`. Never less than one for a non-empty request, so a
    /// tiny window still shows something to click.
    #[must_use]
    pub fn capacity(&self, n: usize) -> usize {
        self.capacity_ending_in(n, MinCell { w: 0, h: 0 })
    }

    /// Like [`Self::capacity`], but the innermost of the `k` cells must also
    /// be at least `last` -- the room the pinned card needs.
    #[must_use]
    pub fn capacity_ending_in(&self, n: usize, last: MinCell) -> usize {
        let mut k = n.min(Self::MAX_CELLS);
        while k > 1 && !self.legible(k, last) {
            k -= 1;
        }
        k
    }

    fn legible(&self, k: usize, last: MinCell) -> bool {
        let cells = self.cells(k);
        let Some((innermost, outer)) = cells.split_last() else {
            return true;
        };
        outer
            .iter()
            .all(|c| c.w >= self.min_side && c.h >= self.min_side)
            && innermost.w >= self.min_side.max(last.w)
            && innermost.h >= self.min_side.max(last.h)
    }

    /// Splits `rest` into the next cell and what remains. Horizontal cuts
    /// alternate left/right and vertical ones top/bottom, which is what makes
    /// the sequence wind inward instead of marching across.
    fn cut(rest: Rect, turn: usize) -> (Rect, Rect) {
        let leading = turn % 4 < 2;
        if rest.w >= rest.h {
            let w = Self::share(rest.w, Self::CUT, Self::CUT_DEN);
            let (cell_x, rest_x) = if leading {
                (rest.x, rest.x + w)
            } else {
                (rest.right() - w, rest.x)
            };
            (
                Rect::new(cell_x, rest.y, w, rest.h),
                Rect::new(rest_x, rest.y, rest.w - w, rest.h),
            )
        } else {
            let h = Self::share(rest.h, Self::CUT, Self::CUT_DEN);
            let (cell_y, rest_y) = if leading {
                (rest.y, rest.y + h)
            } else {
                (rest.bottom() - h, rest.y)
            };
            (
                Rect::new(rest.x, cell_y, rest.w, h),
                Rect::new(rest.x, rest_y, rest.w, rest.h - h),
            )
        }
    }

    fn share(length: i32, numerator: i64, denominator: i64) -> i32 {
        let length = i64::from(length.max(0));
        i32::try_from((length * numerator + denominator / 2) / denominator).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::SpiralLayout;
    use platform::draw::{Point, Rect};

    fn area_of(cells: &[Rect]) -> i64 {
        cells.iter().map(|c| i64::from(c.w) * i64::from(c.h)).sum()
    }

    #[test]
    fn each_cut_takes_two_fifths_of_the_remaining_long_side() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 130, 80), 1).cells(6);
        let sides: Vec<(i32, i32)> = cells.iter().map(|c| (c.w, c.h)).collect();
        assert_eq!(
            sides,
            [(52, 80), (78, 32), (31, 48), (47, 19), (19, 29), (28, 29)]
        );
    }

    /// The last cell takes the whole remainder, so `n` cells still tile
    /// exactly instead of leaving a 3/5-shaped hole.

    #[test]
    fn the_cells_tile_the_area_exactly() {
        for (w, h) in [(1280, 790), (700, 1200), (333, 97), (1, 1)] {
            let area = Rect::new(17, 23, w, h);
            for n in 1..=SpiralLayout::MAX_CELLS {
                let cells = SpiralLayout::new(area, 1).cells(n);
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
    fn cells_shrink_in_rank_order() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 1).cells(10);
        let areas: Vec<i64> = cells
            .iter()
            .map(|c| i64::from(c.w) * i64::from(c.h))
            .collect();
        for pair in areas.windows(2).take(areas.len() - 2) {
            assert!(pair[0] >= pair[1], "{areas:?}");
        }
    }

    #[test]
    fn the_spiral_winds_inward() {
        let cells = SpiralLayout::new(Rect::new(0, 0, 130, 80), 1).cells(6);
        assert_eq!(cells[0].x, 0, "first cell on the left");
        assert_eq!(cells[1].y, 0, "second cell on top");
        assert_eq!(cells[2].right(), 130, "third cell on the right");
        assert_eq!(cells[3].bottom(), 80, "fourth cell on the bottom");
        assert!(cells[5].contains(Point { x: 85, y: 38 }), "{cells:?}");
    }

    #[test]
    fn the_innermost_cell_keeps_the_room_it_was_promised() {
        let layout = SpiralLayout::new(Rect::new(0, 0, 1280, 790), 64);
        let need = super::MinCell { w: 150, h: 200 };
        let k = layout.capacity_ending_in(100, need);
        let cells = layout.cells(k);
        let last = cells.last().expect("cells");
        assert!(last.w >= 150 && last.h >= 200, "{last:?}");
        assert!(k < layout.capacity(100), "the promise costs cells");
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
    fn a_degenerate_area_never_produces_negative_extents() {
        for area in [Rect::new(0, 0, 0, 0), Rect::new(0, 0, -5, 40)] {
            for c in SpiralLayout::new(area, 1).cells(8) {
                assert!(c.w >= 0 && c.h >= 0, "{c:?}");
            }
        }
    }
}
