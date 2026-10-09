//! [`LayoutGate`] -- says when the ranking of the laid-out groups changed.
//!
//! Tile sizes follow rank alone, so memory drift that keeps the order never
//! moves a tile.

use crate::group_smoother::Share;

#[derive(Default)]
pub struct LayoutGate {
    laid: Vec<String>,
}

impl LayoutGate {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&mut self, shares: &[Share]) {
        self.laid = shares.iter().map(|share| share.name.clone()).collect();
    }

    /// Whether the order of the laid-out groups changed or one of them went
    /// away.
    #[must_use]
    pub fn stale(&self, shares: &[Share]) -> bool {
        shares.len() < self.laid.len()
            || self
                .laid
                .iter()
                .zip(shares)
                .any(|(old, new)| *old != new.name)
    }
}

#[cfg(test)]
mod tests {
    use super::LayoutGate;
    use crate::group_smoother::Share;

    fn shares(entries: &[(&str, f64)]) -> Vec<Share> {
        entries
            .iter()
            .map(|&(name, share)| Share {
                name: name.to_owned(),
                share,
            })
            .collect()
    }

    fn gate(entries: &[(&str, f64)]) -> LayoutGate {
        let mut gate = LayoutGate::new();
        gate.remember(&shares(entries));
        gate
    }

    #[test]
    fn drift_that_keeps_the_order_keeps_the_layout() {
        let gate = gate(&[("a", 0.4), ("b", 0.2)]);
        assert!(!gate.stale(&shares(&[("a", 0.43), ("b", 0.19)])));
        assert!(!gate.stale(&shares(&[("a", 0.4), ("b", 0.39)])));
    }

    #[test]
    fn a_reorder_or_a_missing_group_relays_out() {
        let gate = gate(&[("a", 0.4), ("b", 0.39)]);
        assert!(gate.stale(&shares(&[("b", 0.4), ("a", 0.39)])));
        assert!(gate.stale(&shares(&[("a", 0.4)])));
    }

    #[test]
    fn groups_past_the_laid_out_ones_may_change_freely() {
        let gate = gate(&[("a", 0.5)]);
        assert!(!gate.stale(&shares(&[("a", 0.5), ("x", 0.2), ("y", 0.1)])));
        assert!(!gate.stale(&shares(&[("a", 0.5), ("q", 0.3)])));
    }

    #[test]
    fn nothing_laid_out_and_nothing_to_show_is_stable() {
        assert!(!LayoutGate::new().stale(&[]));
    }
}
