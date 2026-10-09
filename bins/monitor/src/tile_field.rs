//! [`TileField`] -- the process tiles' state: grouped and smoothed process
//! data, and the arrangement of tiles, kept steady until it drifts.

use std::collections::HashMap;

use platform::draw::Rect;
use sysmon::ProcessInfo;

use crate::group_smoother::GroupSmoother;
use crate::layout_gate::LayoutGate;
use crate::process_group::{ProcessGroup, ProcessGrouper};
use crate::tile_layout::{Arrangement, TileKind, TileLayout};
use crate::tile_look::TileHeat;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Face {
    name: String,
    heat: u8,
    cpu: u32,
    megabytes: u64,
    count: usize,
}

/// The groups folded into the "more" tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    pub groups: usize,
    pub memory_kb: u64,
}

#[derive(Default)]
pub struct TileField {
    smoother: GroupSmoother,
    gate: LayoutGate,
    groups: HashMap<String, ProcessGroup>,
    faces: Vec<Face>,
    arrangement: Arrangement,
    laid_for: Option<(Rect, i32)>,
    laid_total: usize,
}

impl TileField {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in a finished process sweep. Returns whether anything a tile
    /// shows (order, heat, memory, member count) is different now.
    pub fn accept(&mut self, processes: &[ProcessInfo]) -> bool {
        let groups = ProcessGrouper::group(processes);
        self.smoother.update(&groups);
        self.groups = groups
            .into_iter()
            .map(|group| (group.name.clone(), group))
            .collect();
        let faces = self.faces();
        let changed = faces != self.faces;
        self.faces = faces;
        changed
    }

    fn faces(&self) -> Vec<Face> {
        self.smoother
            .shares()
            .iter()
            .filter_map(|share| {
                let group = self.groups.get(&share.name)?;
                Some(Face {
                    name: share.name.clone(),
                    heat: self.heat(&share.name),
                    cpu: self.cpu_label(&share.name),
                    megabytes: group.memory_kb / 1024,
                    count: group.count,
                })
            })
            .collect()
    }

    /// Lay the tiles out in `area`, unless the current arrangement still
    /// serves. Returns whether the tiles moved or changed.
    pub fn prepare(&mut self, area: Rect, min_side: i32) -> bool {
        let shares = self.smoother.shares();
        let total = shares.len();
        let shown = self.arrangement.shown;
        let had_more = shown < self.laid_total;
        let more_toggled = if had_more {
            total <= shown
        } else {
            total > shown
        };
        let stale =
            self.laid_for != Some((area, min_side)) || more_toggled || self.gate.stale(shares);
        if stale {
            self.arrangement = TileLayout::new(area, min_side).arrange(shares);
            self.laid_for = Some((area, min_side));
            self.laid_total = total;
            self.gate.remember(&shares[..self.arrangement.shown]);
            return true;
        }
        let tail = total - shown;
        let mut patched = false;
        for tile in &mut self.arrangement.tiles {
            if let TileKind::More { count } = &mut tile.kind {
                patched = *count != tail;
                *count = tail;
            }
        }
        self.laid_total = total;
        patched
    }

    #[must_use]
    pub const fn arrangement(&self) -> &Arrangement {
        &self.arrangement
    }

    #[must_use]
    pub fn group(&self, name: &str) -> Option<&ProcessGroup> {
        self.groups.get(name)
    }

    #[must_use]
    pub fn cpu(&self, name: &str) -> f64 {
        self.smoother.cpu(name).unwrap_or(0.0)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn cpu_label(&self, name: &str) -> u32 {
        self.cpu(name).round().max(0.0) as u32
    }

    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn heat(&self, name: &str) -> u8 {
        TileHeat::of(self.cpu(name) as f32)
    }

    #[must_use]
    pub fn tail(&self) -> Tail {
        let shares = self.smoother.shares();
        let rest = shares.get(self.arrangement.shown..).unwrap_or_default();
        Tail {
            groups: rest.len(),
            memory_kb: rest
                .iter()
                .filter_map(|share| self.groups.get(&share.name))
                .map(|group| group.memory_kb)
                .sum(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileField;
    use crate::tile_layout::TileKind;
    use platform::draw::Rect;
    use sysmon::ProcessInfo;

    const AREA: Rect = Rect::new(18, 18, 1300, 800);

    fn process(pid: u32, name: &str, cpu: f32, memory_kb: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.to_owned(),
            cpu,
            memory_kb,
        }
    }

    fn many(n: u32) -> Vec<ProcessInfo> {
        (0..n)
            .map(|i| process(i, &format!("app{i}"), 0.0, 1_000_000 / u64::from(i + 1)))
            .collect()
    }

    fn process_tiles(field: &TileField) -> Vec<String> {
        field
            .arrangement()
            .tiles
            .iter()
            .filter_map(|tile| match &tile.kind {
                TileKind::Process(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_family_becomes_one_tile() {
        let mut field = TileField::new();
        field.accept(&[
            process(1, "kworker/0:1", 1.0, 2000),
            process(2, "kworker/u8:3-events", 1.0, 2000),
            process(3, "kworker/R-rcu_gp", 1.0, 2000),
            process(4, "firefox", 5.0, 4096),
        ]);
        field.prepare(AREA, 1);
        let names = process_tiles(&field);
        assert_eq!(names.iter().filter(|n| *n == "kworker").count(), 1);
        assert_eq!(field.group("kworker").map(|g| g.count), Some(3));
    }

    #[test]
    fn an_identical_sweep_changes_nothing() {
        let mut field = TileField::new();
        let list = many(10);
        assert!(field.accept(&list));
        assert!(!field.accept(&list));
    }

    #[test]
    fn new_heat_is_a_change_but_tiny_noise_is_not() {
        let mut field = TileField::new();
        field.accept(&[process(1, "a", 0.0, 4096)]);
        assert!(!field.accept(&[process(1, "a", 0.01, 4096)]));
        assert!(field.accept(&[process(1, "a", 90.0, 4096)]));
    }

    #[test]
    fn the_arrangement_holds_until_the_area_changes() {
        let mut field = TileField::new();
        field.accept(&many(30));
        assert!(field.prepare(AREA, 100));
        assert!(!field.prepare(AREA, 100));
        assert!(field.prepare(Rect::new(18, 18, 900, 700), 100));
    }

    #[test]
    fn a_small_drift_keeps_the_arrangement_and_a_big_one_relays_out() {
        let mut field = TileField::new();
        field.accept(&many(30));
        field.prepare(AREA, 100);
        let before = field.arrangement().clone();
        field.accept(&many(30));
        assert!(!field.prepare(AREA, 100));
        assert_eq!(field.arrangement(), &before);

        let mut swapped = many(30);
        swapped[0].memory_kb = 1;
        swapped[29].memory_kb = 5_000_000;
        for _ in 0..12 {
            field.accept(&swapped);
        }
        assert!(field.prepare(AREA, 100));
        assert_ne!(field.arrangement(), &before);
    }

    #[test]
    fn the_more_tile_follows_the_group_count_without_moving_tiles() {
        let mut field = TileField::new();
        field.accept(&many(40));
        field.prepare(AREA, 110);
        let shown = field.arrangement().shown;
        assert!(shown < 40);
        let rects: Vec<Rect> = field.arrangement().tiles.iter().map(|t| t.rect).collect();

        field.accept(&many(42));
        assert!(field.prepare(AREA, 110));
        let after: Vec<Rect> = field.arrangement().tiles.iter().map(|t| t.rect).collect();
        assert_eq!(rects, after);
        let count = field.arrangement().tiles.iter().find_map(|t| match t.kind {
            TileKind::More { count } => Some(count),
            _ => None,
        });
        assert_eq!(count, Some(42 - shown));
        assert_eq!(field.tail().groups, 42 - shown);
    }

    #[test]
    fn the_tail_sums_the_memory_of_hidden_groups() {
        let mut field = TileField::new();
        field.accept(&many(40));
        field.prepare(AREA, 110);
        let shown = field.arrangement().shown;
        let expected: u64 = (shown..40)
            .map(|i| 1_000_000 / u64::try_from(i + 1).unwrap_or(1))
            .sum();
        assert_eq!(field.tail().memory_kb, expected);
    }

    #[test]
    fn an_empty_system_still_lays_out_the_fixed_tiles() {
        let mut field = TileField::new();
        assert!(field.prepare(AREA, 100));
        assert_eq!(field.arrangement().tiles.len(), 5);
        assert_eq!(field.tail().groups, 0);
    }
}
