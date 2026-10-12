//! [`TileField`] -- the process tiles' state: grouped and smoothed process
//! data, which group is expanded and which page is open, and the arrangement
//! of tiles, kept steady until it drifts.

use platform::draw::{Point, Rect};
use sysmon::ProcessInfo;

use crate::group_smoother::Share;
use crate::layout_gate::LayoutGate;
use crate::process_group::{ProcessGroup, ProcessGrouper};
use crate::process_level::ProcessLevel;
use crate::tile_layout::{Arrangement, TileKind, TileLayout};
use crate::tile_look::TileHeat;
use crate::tile_minimum::TileMinimum;
use crate::tile_view::TileView;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Face {
    name: String,
    heat: u8,
    cpu: u32,
    megabytes: u64,
    count: usize,
}

/// The entries folded into the "more" tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    pub groups: usize,
    pub memory_kb: u64,
}

#[derive(Default)]
pub struct TileField {
    processes: Vec<ProcessInfo>,
    root: ProcessLevel,
    members: ProcessLevel,
    view: TileView,
    gate: LayoutGate,
    faces: Vec<Face>,
    arrangement: Arrangement,
    laid_for: Option<(Rect, TileMinimum, TileView)>,
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
        self.processes = processes.to_vec();
        self.root.update(ProcessGrouper::group(processes));
        let mut moved = false;
        if let Some(group) = self.view.group().map(str::to_owned) {
            let members = ProcessGrouper::members(processes, &group);
            if members.is_empty() {
                moved = self.view.leave_group();
            } else {
                self.members.update(members);
            }
        }
        moved |= self.view.clamp(self.level().shares().len());
        self.refresh_faces() || moved
    }

    fn refresh_faces(&mut self) -> bool {
        let faces = self.faces();
        let changed = faces != self.faces;
        self.faces = faces;
        changed
    }

    fn level(&self) -> &ProcessLevel {
        if self.view.group().is_some() {
            &self.members
        } else {
            &self.root
        }
    }

    fn page_shares(&self) -> &[Share] {
        self.level()
            .shares()
            .get(self.view.start()..)
            .unwrap_or_default()
    }

    fn faces(&self) -> Vec<Face> {
        self.page_shares()
            .iter()
            .filter_map(|share| {
                let group = self.group(&share.name)?;
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
    pub fn prepare(&mut self, area: Rect, minimum: TileMinimum) -> bool {
        let shares = self.page_shares();
        let total = shares.len();
        let shown = self.arrangement.shown;
        let had_more = shown < self.laid_total;
        let more_toggled = if had_more {
            total <= shown
        } else {
            total > shown
        };
        let placed = (area, minimum, self.view.clone());
        let stale =
            self.laid_for.as_ref() != Some(&placed) || more_toggled || self.gate.stale(shares);
        if stale {
            let arrangement = TileLayout::new(area, minimum).arrange(shares);
            let mut gate = LayoutGate::new();
            gate.remember(&shares[..arrangement.shown]);
            self.arrangement = arrangement;
            self.gate = gate;
            self.laid_for = Some(placed);
            self.laid_total = total;
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
    pub const fn view(&self) -> &TileView {
        &self.view
    }

    #[must_use]
    pub fn group(&self, name: &str) -> Option<&ProcessGroup> {
        self.level().group(name)
    }

    #[must_use]
    pub fn cpu(&self, name: &str) -> f64 {
        self.level().cpu(name)
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
        let rest = self
            .page_shares()
            .get(self.arrangement.shown..)
            .unwrap_or_default();
        Tail {
            groups: rest.len(),
            memory_kb: rest
                .iter()
                .filter_map(|share| self.group(&share.name))
                .map(|group| group.memory_kb)
                .sum(),
        }
    }

    /// A click at `at`: the "more" tile slides the field on by one, a group
    /// of several processes opens onto its members. Returns whether the view
    /// changed.
    pub fn click(&mut self, at: Point) -> bool {
        let Some(kind) = self.arrangement.at(at).map(|tile| tile.kind.clone()) else {
            return false;
        };
        let changed = match kind {
            TileKind::More { .. } => self.slide(1),
            TileKind::Process(name) if self.view.group().is_none() => self.expand(&name),
            _ => false,
        };
        changed && self.refresh_after_move()
    }

    /// A wheel turn of `lines`: each line slides one tile, positive dropping
    /// the largest in view and bringing in the next smaller. Returns whether
    /// the view changed.
    pub fn wheel(&mut self, lines: i32) -> bool {
        self.slide(lines) && self.refresh_after_move()
    }

    /// Slide back to the largest entries, or else leave the expanded group.
    pub fn back(&mut self) -> bool {
        self.view.back() && self.refresh_after_move()
    }

    fn refresh_after_move(&mut self) -> bool {
        self.refresh_faces();
        true
    }

    fn slide(&mut self, steps: i32) -> bool {
        let later = self.tail().groups > 0;
        self.view.slide(steps, later)
    }

    fn expand(&mut self, name: &str) -> bool {
        let has_members = self.root.group(name).is_some_and(|group| group.count > 1);
        if !has_members || !self.view.expand(name) {
            return false;
        }
        self.members = ProcessLevel::new(ProcessGrouper::members(&self.processes, name));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::TileField;
    use crate::tile_layout::TileKind;
    use crate::tile_minimum::TileMinimum;
    use platform::draw::{Point, Rect};
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

    fn centre_of(field: &TileField, wanted: impl Fn(&TileKind) -> bool) -> Point {
        let rect = field
            .arrangement()
            .tiles
            .iter()
            .find(|tile| wanted(&tile.kind))
            .expect("tile shown")
            .rect;
        Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2)
    }

    fn with_family() -> Vec<ProcessInfo> {
        let mut list = many(6);
        list.extend((0..5).map(|i| process(100 + i, "helium", 1.0, 900_000 + u64::from(i))));
        list
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
        field.prepare(AREA, TileMinimum::square(1));
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
        assert!(field.prepare(AREA, TileMinimum::square(100)));
        assert!(!field.prepare(AREA, TileMinimum::square(100)));
        assert!(field.prepare(Rect::new(18, 18, 900, 700), TileMinimum::square(100)));
    }

    #[test]
    fn a_small_drift_keeps_the_arrangement_and_a_big_one_relays_out() {
        let mut field = TileField::new();
        field.accept(&many(30));
        field.prepare(AREA, TileMinimum::square(100));
        let before = field.arrangement().clone();
        field.accept(&many(30));
        assert!(!field.prepare(AREA, TileMinimum::square(100)));
        assert_eq!(field.arrangement(), &before);

        let mut swapped = many(30);
        swapped[0].memory_kb = 1;
        swapped[29].memory_kb = 5_000_000;
        for _ in 0..12 {
            field.accept(&swapped);
        }
        assert!(field.prepare(AREA, TileMinimum::square(100)));
        assert_ne!(field.arrangement(), &before);
    }

    #[test]
    fn the_more_tile_follows_the_group_count_without_moving_tiles() {
        let mut field = TileField::new();
        field.accept(&many(90));
        field.prepare(AREA, TileMinimum::square(110));
        let shown = field.arrangement().shown;
        assert!(shown < 90);
        let rects: Vec<Rect> = field.arrangement().tiles.iter().map(|t| t.rect).collect();

        field.accept(&many(92));
        assert!(field.prepare(AREA, TileMinimum::square(110)));
        let after: Vec<Rect> = field.arrangement().tiles.iter().map(|t| t.rect).collect();
        assert_eq!(rects, after);
        let count = field.arrangement().tiles.iter().find_map(|t| match t.kind {
            TileKind::More { count } => Some(count),
            _ => None,
        });
        assert_eq!(count, Some(92 - shown));
        assert_eq!(field.tail().groups, 92 - shown);
    }

    #[test]
    fn the_tail_sums_the_memory_of_hidden_groups() {
        let mut field = TileField::new();
        field.accept(&many(40));
        field.prepare(AREA, TileMinimum::square(110));
        let shown = field.arrangement().shown;
        let expected: u64 = (shown..40)
            .map(|i| 1_000_000 / u64::try_from(i + 1).unwrap_or(1))
            .sum();
        assert_eq!(field.tail().memory_kb, expected);
    }

    #[test]
    fn an_empty_system_still_lays_out_the_fixed_tiles() {
        let mut field = TileField::new();
        assert!(field.prepare(AREA, TileMinimum::square(100)));
        assert_eq!(field.arrangement().tiles.len(), 5);
        assert_eq!(field.tail().groups, 0);
    }

    #[test]
    fn clicking_more_slides_one_tile_and_back_returns_to_the_top() {
        let mut field = TileField::new();
        field.accept(&many(60));
        field.prepare(AREA, TileMinimum::square(110));
        let more = centre_of(&field, |k| matches!(k, TileKind::More { .. }));
        assert!(field.click(more));
        field.prepare(AREA, TileMinimum::square(110));
        assert_eq!(process_tiles(&field)[0], "app1");
        assert!(field.back());
        field.prepare(AREA, TileMinimum::square(110));
        assert_eq!(process_tiles(&field)[0], "app0");
    }

    #[test]
    fn each_wheel_line_moves_one_tile_out_and_one_in() {
        let mut field = TileField::new();
        field.accept(&many(60));
        field.prepare(AREA, TileMinimum::square(110));
        assert!(!field.wheel(-1), "already at the largest");
        assert!(!field.wheel(0));
        let before = process_tiles(&field);
        assert!(field.wheel(1));
        field.prepare(AREA, TileMinimum::square(110));
        let after = process_tiles(&field);
        assert_eq!(after[0], before[1], "the largest left");
        assert!(
            !before.contains(after.last().expect("tiles")),
            "a smaller one came in"
        );
        let mut turned = 1;
        while field.wheel(1) {
            field.prepare(AREA, TileMinimum::square(110));
            turned += 1;
            assert!(turned < 60, "the wheel must reach the end");
        }
        assert_eq!(field.tail().groups, 0);
        while field.wheel(-3) {
            field.prepare(AREA, TileMinimum::square(110));
        }
        assert_eq!(field.view().start(), 0);
        assert_eq!(process_tiles(&field)[0], "app0");
    }

    #[test]
    fn clicking_a_family_shows_its_members_and_back_returns() {
        let mut field = TileField::new();
        field.accept(&with_family());
        field.prepare(AREA, TileMinimum::square(110));
        let helium = centre_of(&field, |k| *k == TileKind::Process("helium".to_owned()));
        assert!(field.click(helium));
        assert!(field.prepare(AREA, TileMinimum::square(110)));
        let members = process_tiles(&field);
        assert_eq!(members.len(), 5);
        assert_eq!(members[0], "helium 104");
        assert_eq!(field.group("helium 104").map(|g| g.count), Some(1));
        let member = centre_of(&field, |k| matches!(k, TileKind::Process(_)));
        assert!(!field.click(member), "members do not expand further");
        assert!(field.back());
        field.prepare(AREA, TileMinimum::square(110));
        assert!(process_tiles(&field).contains(&"helium".to_owned()));
    }

    #[test]
    fn a_single_process_does_not_expand() {
        let mut field = TileField::new();
        field.accept(&with_family());
        field.prepare(AREA, TileMinimum::square(110));
        let lone = centre_of(&field, |k| *k == TileKind::Process("app0".to_owned()));
        assert!(!field.click(lone));
        assert!(!field.click(centre_of(&field, |k| *k == TileKind::Info)));
    }

    #[test]
    fn an_expanded_family_that_exits_returns_to_the_groups() {
        let mut field = TileField::new();
        field.accept(&with_family());
        field.prepare(AREA, TileMinimum::square(110));
        field.click(centre_of(&field, |k| {
            *k == TileKind::Process("helium".to_owned())
        }));
        assert!(field.view().group().is_some());
        assert!(field.accept(&many(6)));
        assert!(field.view().group().is_none());
    }

    #[test]
    fn members_follow_the_sweep() {
        let mut field = TileField::new();
        field.accept(&with_family());
        field.prepare(AREA, TileMinimum::square(110));
        field.click(centre_of(&field, |k| {
            *k == TileKind::Process("helium".to_owned())
        }));
        field.prepare(AREA, TileMinimum::square(110));
        let mut next = with_family();
        next.retain(|p| p.pid != 104);
        assert!(field.accept(&next));
        field.prepare(AREA, TileMinimum::square(110));
        assert_eq!(process_tiles(&field).len(), 4);
    }
}
