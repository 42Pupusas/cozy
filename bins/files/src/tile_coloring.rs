//! [`TileColoring`] -- gives each tile one of [`TileColoring::COLORS`] fill
//! slots so that no two tiles sharing an edge share a slot.

use platform::draw::Rect;

pub struct TileColoring {
    neighbours: Vec<Vec<usize>>,
}

impl TileColoring {
    pub const COLORS: usize = 4;

    const SEARCH_BUDGET: usize = 200_000;

    #[must_use]
    pub fn of(cells: &[Rect]) -> Self {
        let neighbours = cells
            .iter()
            .enumerate()
            .map(|(i, a)| {
                cells
                    .iter()
                    .enumerate()
                    .filter(|&(j, b)| j != i && Self::borders(*a, *b))
                    .map(|(j, _)| j)
                    .collect()
            })
            .collect();
        Self { neighbours }
    }

    /// One slot per cell, in cell order. Cells meeting only at a corner may
    /// share a slot.
    #[must_use]
    pub fn slots(&self) -> Vec<usize> {
        let mut slots = vec![0; self.neighbours.len()];
        let mut budget = Self::SEARCH_BUDGET;
        if self.assign(0, &mut slots, &mut budget) {
            slots
        } else {
            (0..self.neighbours.len())
                .map(|cell| cell % Self::COLORS)
                .collect()
        }
    }

    #[must_use]
    pub fn borders(a: Rect, b: Rect) -> bool {
        let abut_x = a.right() == b.x || b.right() == a.x;
        let abut_y = a.bottom() == b.y || b.bottom() == a.y;
        let overlap_x = a.right().min(b.right()) > a.x.max(b.x);
        let overlap_y = a.bottom().min(b.bottom()) > a.y.max(b.y);
        (abut_x && overlap_y) || (abut_y && overlap_x)
    }

    fn assign(&self, cell: usize, slots: &mut [usize], budget: &mut usize) -> bool {
        if cell == slots.len() {
            return true;
        }
        for step in 0..Self::COLORS {
            let slot = (cell + step) % Self::COLORS;
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            if self.neighbours[cell]
                .iter()
                .any(|&other| other < cell && slots[other] == slot)
            {
                continue;
            }
            slots[cell] = slot;
            if self.assign(cell + 1, slots, budget) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::TileColoring;
    use crate::spiral::SpiralLayout;
    use platform::draw::Rect;

    fn assert_proper(cells: &[Rect]) {
        let coloring = TileColoring::of(cells);
        let slots = coloring.slots();
        assert_eq!(slots.len(), cells.len());
        assert!(slots.iter().all(|&slot| slot < TileColoring::COLORS));
        for (i, others) in coloring.neighbours.iter().enumerate() {
            for &j in others {
                assert_ne!(slots[i], slots[j], "{:?} and {:?}", cells[i], cells[j]);
            }
        }
    }

    #[test]
    fn spiral_neighbours_never_share_a_slot() {
        for (w, h) in [(1280, 790), (700, 1200), (333, 97), (1000, 1000), (97, 333)] {
            for n in 1..=SpiralLayout::MAX_CELLS {
                let cells = SpiralLayout::new(Rect::new(17, 23, w, h), 1).cells(n);
                assert_proper(&cells);
            }
        }
    }

    #[test]
    fn four_or_more_cells_use_every_slot_when_the_layout_allows() {
        for (w, h) in [(1280, 790), (700, 1200), (1000, 1000)] {
            for n in 4..=SpiralLayout::MAX_CELLS {
                let cells = SpiralLayout::new(Rect::new(0, 0, w, h), 1).cells(n);
                let slots = TileColoring::of(&cells).slots();
                let used: std::collections::BTreeSet<usize> = slots.into_iter().collect();
                assert_eq!(used.len(), TileColoring::COLORS, "{n} cells in {w}x{h}");
            }
        }
    }

    #[test]
    fn a_lone_cell_takes_the_first_slot() {
        let cells = [Rect::new(0, 0, 100, 100)];
        assert_eq!(TileColoring::of(&cells).slots(), [0]);
    }

    #[test]
    fn nothing_to_colour_yields_nothing() {
        assert!(TileColoring::of(&[]).slots().is_empty());
    }

    #[test]
    fn cells_sharing_an_edge_are_neighbours() {
        let cells = [Rect::new(0, 0, 50, 50), Rect::new(50, 10, 50, 20)];
        let coloring = TileColoring::of(&cells);
        assert_eq!(coloring.neighbours[0], [1]);
        assert_eq!(coloring.neighbours[1], [0]);
    }

    #[test]
    fn cells_meeting_at_a_corner_are_not_neighbours() {
        let cells = [Rect::new(0, 0, 50, 50), Rect::new(50, 50, 50, 50)];
        let coloring = TileColoring::of(&cells);
        assert!(coloring.neighbours.iter().all(Vec::is_empty));
        assert_eq!(coloring.slots(), [0, 1]);
    }

    #[test]
    fn a_gap_between_cells_separates_them() {
        let cells = [Rect::new(0, 0, 50, 50), Rect::new(52, 0, 50, 50)];
        assert!(
            TileColoring::of(&cells)
                .neighbours
                .iter()
                .all(Vec::is_empty)
        );
    }

    #[test]
    fn a_grid_where_every_cell_touches_four_others_still_needs_few_slots() {
        let cells: Vec<Rect> = (0..16)
            .map(|k| Rect::new((k % 4) * 10, (k / 4) * 10, 10, 10))
            .collect();
        assert_proper(&cells);
        let slots = TileColoring::of(&cells).slots();
        assert!(slots.iter().all(|&slot| slot < 4));
    }

    #[test]
    fn a_pinwheel_around_a_centre_is_properly_coloured() {
        let cells = [
            Rect::new(0, 0, 20, 10),
            Rect::new(20, 0, 10, 20),
            Rect::new(10, 20, 20, 10),
            Rect::new(0, 10, 10, 20),
            Rect::new(10, 10, 10, 10),
        ];
        assert_proper(&cells);
    }
}
