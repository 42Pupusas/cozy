//! [`TileRank`] -- which entries get a spiral cell, in what order, and how
//! the rest collapse into a single overflow tile.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    /// An index into the browser's visible entries.
    Item(usize),
    /// Ranks `from..from + count`, opened as their own spiral.
    More { from: usize, count: usize },
}

impl Tile {
    /// The visible-entry index, for an entry tile.
    #[must_use]
    pub const fn item(self) -> Option<usize> {
        match self {
            Self::Item(index) => Some(index),
            Self::More { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileRank {
    /// Visible-entry indices, largest first; ties keep visible order.
    order: Vec<usize>,
}

impl TileRank {
    #[must_use]
    pub fn new(sizes: &[u64]) -> Self {
        let mut order: Vec<usize> = (0..sizes.len()).collect();
        order.sort_by(|&a, &b| sizes[b].cmp(&sizes[a]));
        Self { order }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.order.len()
    }

    /// Visible-entry indices, largest first.
    #[must_use]
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The tiles for the page starting at rank `from`, given room for
    /// `capacity` cells. The last cell becomes [`Tile::More`] only when the
    /// remaining entries do not all fit. An overflowing page always shows at
    /// least one entry, so every `More` tile leads strictly further on.
    #[must_use]
    pub fn page(&self, from: usize, capacity: usize) -> Vec<Tile> {
        let rest = self.order.get(from..).unwrap_or_default();
        if capacity == 0 || rest.is_empty() {
            return Vec::new();
        }
        if rest.len() <= capacity {
            return rest.iter().map(|&i| Tile::Item(i)).collect();
        }
        let shown = capacity.max(2) - 1;
        let mut tiles: Vec<Tile> = rest[..shown].iter().map(|&i| Tile::Item(i)).collect();
        tiles.push(Tile::More {
            from: from + shown,
            count: rest.len() - shown,
        });
        tiles
    }
}

#[cfg(test)]
mod tests {
    use super::{Tile, TileRank};

    #[test]
    fn entries_are_ranked_largest_first() {
        let rank = TileRank::new(&[10, 500, 30]);
        assert_eq!(
            rank.page(0, 8),
            [Tile::Item(1), Tile::Item(2), Tile::Item(0)]
        );
    }

    #[test]
    fn ties_keep_visible_order() {
        let rank = TileRank::new(&[7, 7, 7]);
        assert_eq!(
            rank.page(0, 8),
            [Tile::Item(0), Tile::Item(1), Tile::Item(2)]
        );
    }

    #[test]
    fn an_exact_fit_has_no_overflow_tile() {
        let rank = TileRank::new(&[3, 2, 1]);
        assert_eq!(rank.page(0, 3).len(), 3);
        assert!(
            !rank
                .page(0, 3)
                .iter()
                .any(|t| matches!(t, Tile::More { .. }))
        );
    }

    #[test]
    fn the_tail_collapses_into_the_last_cell() {
        let rank = TileRank::new(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(
            rank.page(0, 3),
            [
                Tile::Item(0),
                Tile::Item(1),
                Tile::More { from: 2, count: 4 }
            ]
        );
    }

    #[test]
    fn an_overflow_page_continues_where_the_last_stopped() {
        let rank = TileRank::new(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(
            rank.page(2, 3),
            [
                Tile::Item(2),
                Tile::Item(3),
                Tile::More { from: 4, count: 2 }
            ]
        );
        assert_eq!(rank.page(4, 3), [Tile::Item(4), Tile::Item(5)]);
    }

    #[test]
    fn every_entry_is_reachable_through_overflow_pages() {
        let sizes: Vec<u64> = (0..100).collect();
        let rank = TileRank::new(&sizes);
        let mut seen = Vec::new();
        let mut from = 0;
        loop {
            let page = rank.page(from, 5);
            let mut next = None;
            for tile in page {
                match tile {
                    Tile::Item(i) => seen.push(i),
                    Tile::More { from, .. } => next = Some(from),
                }
            }
            match next {
                Some(n) => from = n,
                None => break,
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn an_overflow_page_always_advances() {
        let rank = TileRank::new(&[3, 2, 1]);
        assert_eq!(
            rank.page(0, 1),
            [Tile::Item(0), Tile::More { from: 1, count: 2 }]
        );
    }

    #[test]
    fn the_order_lists_visible_indices_largest_first() {
        let rank = TileRank::new(&[10, 500, 30]);
        assert_eq!(rank.order(), [1, 2, 0]);
        assert_eq!(rank.len(), 3);
        assert_eq!(TileRank::new(&[]).len(), 0);
    }

    #[test]
    fn empty_inputs_yield_no_tiles() {
        assert!(TileRank::new(&[]).page(0, 5).is_empty());
        assert!(TileRank::new(&[1]).page(0, 0).is_empty());
        assert!(TileRank::new(&[1]).page(9, 5).is_empty());
    }
}
