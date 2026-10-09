//! [`TileRank`] -- which entries get a spiral cell, and in what order.

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

    /// The `capacity` entries starting at rank `from`.
    #[must_use]
    pub fn window(&self, from: usize, capacity: usize) -> &[usize] {
        let rest = self.order.get(from..).unwrap_or_default();
        &rest[..capacity.min(rest.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::TileRank;

    #[test]
    fn entries_are_ranked_largest_first() {
        let rank = TileRank::new(&[10, 500, 30]);
        assert_eq!(rank.window(0, 8), [1, 2, 0]);
    }

    #[test]
    fn ties_keep_visible_order() {
        let rank = TileRank::new(&[7, 7, 7]);
        assert_eq!(rank.window(0, 8), [0, 1, 2]);
    }

    #[test]
    fn a_window_holds_at_most_its_capacity() {
        let rank = TileRank::new(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(rank.window(0, 3), [0, 1, 2]);
    }

    #[test]
    fn a_window_starts_at_its_rank_and_ends_with_the_list() {
        let rank = TileRank::new(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(rank.window(2, 3), [2, 3, 4]);
        assert_eq!(rank.window(4, 3), [4, 5]);
    }

    #[test]
    fn sliding_by_one_drops_the_largest_and_takes_the_next_smaller() {
        let rank = TileRank::new(&[6, 5, 4, 3, 2, 1]);
        assert_eq!(rank.window(0, 4), [0, 1, 2, 3]);
        assert_eq!(rank.window(1, 4), [1, 2, 3, 4]);
    }

    #[test]
    fn every_entry_is_reachable_by_sliding() {
        let sizes: Vec<u64> = (0..100).collect();
        let rank = TileRank::new(&sizes);
        let mut seen = Vec::new();
        for from in 0..=95 {
            seen.extend_from_slice(rank.window(from, 5));
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn the_length_counts_every_entry() {
        assert_eq!(TileRank::new(&[10, 500, 30]).len(), 3);
        assert_eq!(TileRank::new(&[]).len(), 0);
    }

    #[test]
    fn empty_inputs_yield_no_tiles() {
        assert!(TileRank::new(&[]).window(0, 5).is_empty());
        assert!(TileRank::new(&[1]).window(0, 0).is_empty());
        assert!(TileRank::new(&[1]).window(9, 5).is_empty());
    }
}
