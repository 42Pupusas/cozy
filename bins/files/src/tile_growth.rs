//! [`TileGrowth`] -- tiles growing out of the spiral and gliding between its
//! cells.
//!
//! Every tile is followed by name, not by position. A tile met for the first
//! time grows from the centre of its cell; one whose cell changed (the
//! ranking moved, or the window slid along it) glides from where it was drawn
//! to the new cell; one that is no longer wanted shrinks away where it
//! stood. A new folder starts the whole field over, its tiles arriving one
//! after another in spiral order.

use std::path::{Path, PathBuf};

use platform::draw::Rect;

use crate::fade::Fade;
use crate::tile_id::TileId;

/// Where the layout wants a tile to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Destination {
    pub id: TileId,
    pub row: usize,
    pub rect: Rect,
    pub slot: usize,
}

/// How a tile is drawn this frame: the cell it occupies right now, which
/// fill it blends between, and how much of the cell it has grown to fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub rect: Rect,
    pub slot: usize,
    pub from_slot: usize,
    pub mix: u8,
    pub grown: u8,
}

impl Shape {
    #[must_use]
    pub const fn resting(rect: Rect, slot: usize) -> Self {
        Self {
            rect,
            slot,
            from_slot: slot,
            mix: u8::MAX,
            grown: u8::MAX,
        }
    }
}

struct Body {
    id: TileId,
    row: usize,
    from: Rect,
    to: Rect,
    from_slot: usize,
    to_slot: usize,
    travel: Fade,
    grown: Fade,
    wanted: bool,
    start_ms: u64,
}

impl Body {
    const fn arriving(dest: &Destination, start_ms: u64) -> Self {
        Self {
            id: dest.id,
            row: dest.row,
            from: dest.rect,
            to: dest.rect,
            from_slot: dest.slot,
            to_slot: dest.slot,
            travel: Fade::full(),
            grown: Fade::rising(),
            wanted: true,
            start_ms,
        }
    }

    fn rect(&self) -> Rect {
        let level = i32::from(self.travel.braking_level());
        let lerp =
            |from: i32, to: i32| from + ((to - from) * level + 127 * (to - from).signum()) / 255;
        Rect::new(
            lerp(self.from.x, self.to.x),
            lerp(self.from.y, self.to.y),
            lerp(self.from.w, self.to.w),
            lerp(self.from.h, self.to.h),
        )
    }

    fn shape(&self) -> Shape {
        let grown = self.grown.level();
        Shape {
            rect: self.rect(),
            slot: self.to_slot,
            from_slot: self.from_slot,
            mix: self.travel.braking_level(),
            grown,
        }
    }

    fn follow(&mut self, dest: &Destination) {
        self.row = dest.row;
        if !self.wanted {
            self.wanted = true;
            self.grown.aim(true);
        }
        if self.to != dest.rect || self.to_slot != dest.slot {
            self.from_slot = if self.travel.braking_level() >= 128 {
                self.to_slot
            } else {
                self.from_slot
            };
            self.from = self.rect();
            self.to = dest.rect;
            self.to_slot = dest.slot;
            self.travel = Fade::rising();
        }
    }

    const fn release(&mut self) {
        self.wanted = false;
        self.start_ms = 0;
        self.grown.aim(false);
    }

    fn step(&mut self, now_ms: u64, elapsed_ms: u16) {
        if now_ms >= self.start_ms {
            self.grown.step(elapsed_ms, TileGrowth::GROW_MS);
        }
        self.travel.step(elapsed_ms, TileGrowth::MOVE_MS);
    }

    const fn settle(&mut self) {
        self.travel = Fade::full();
        self.from = self.to;
        self.from_slot = self.to_slot;
    }

    fn is_settled(&self) -> bool {
        self.grown.is_settled() && self.travel.is_settled()
    }

    fn is_gone(&self) -> bool {
        !self.wanted && self.grown.is_gone()
    }
}

pub struct TileGrowth {
    scene: Option<PathBuf>,
    bounds: Option<Rect>,
    bodies: Vec<Body>,
    clock: Option<u64>,
}

impl TileGrowth {
    const GROW_MS: u16 = 280;
    const MOVE_MS: u16 = 260;
    const MAX_STEP_MS: u64 = 64;
    const STAGGER_MS: u64 = 55;
    const LONGEST_STAGGER_MS: u64 = 500;
    const TEXT_FROM: u16 = 220;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            scene: None,
            bounds: None,
            bodies: Vec::new(),
            clock: None,
        }
    }

    /// Whether any scene has been aimed at yet. Before that nothing is
    /// followed, and tiles are drawn where the layout puts them.
    #[must_use]
    pub const fn is_primed(&self) -> bool {
        self.scene.is_some()
    }

    /// Point the tiles at `targets`, the field's tiles in spiral order, in
    /// the folder `scene` laid out inside `bounds`. A change of `bounds`
    /// (the window being resized) moves the tiles at once rather than
    /// having them trail behind the edge being dragged.
    pub fn aim(&mut self, scene: &Path, bounds: Rect, targets: &[Destination], now_ms: u64) {
        let entrance = self.scene.as_deref() != Some(scene);
        if entrance {
            self.bodies.clear();
            self.clock = None;
            self.scene = Some(scene.to_path_buf());
        }
        let resized = self.bounds.is_some_and(|seen| seen != bounds);
        self.bounds = Some(bounds);
        let gap = Self::gap_ms(targets.len());
        for body in &mut self.bodies {
            if body.wanted && !targets.iter().any(|dest| dest.id == body.id) {
                body.release();
            }
        }
        for (index, dest) in targets.iter().enumerate() {
            if let Some(body) = self.bodies.iter_mut().find(|body| body.id == dest.id) {
                body.follow(dest);
            } else {
                let delay = if entrance {
                    gap * u64::try_from(index).unwrap_or(u64::MAX)
                } else {
                    0
                };
                self.bodies.push(Body::arriving(dest, now_ms + delay));
            }
        }
        if resized {
            self.settle();
        }
        if self.clock.is_none() && !self.is_settled() {
            self.clock = Some(now_ms);
        }
    }

    fn gap_ms(count: usize) -> u64 {
        let count = u64::try_from(count).unwrap_or(u64::MAX).max(1);
        Self::STAGGER_MS
            .min(Self::LONGEST_STAGGER_MS / count)
            .max(1)
    }

    pub fn advance(&mut self, now_ms: u64) {
        let Some(then) = self.clock else {
            return;
        };
        let elapsed = now_ms.saturating_sub(then).min(Self::MAX_STEP_MS);
        let elapsed = u16::try_from(elapsed).unwrap_or(u16::MAX);
        self.clock = Some(now_ms.max(then));
        for body in &mut self.bodies {
            body.step(now_ms, elapsed);
        }
        self.bodies.retain(|body| !body.is_gone());
        if self.is_settled() {
            self.clock = None;
        }
    }

    /// Land every glide where it is going, for a change that should not be
    /// followed, such as the window being resized.
    pub fn settle(&mut self) {
        for body in &mut self.bodies {
            body.settle();
        }
    }

    /// How the tile named `id` is drawn, when it is being followed.
    #[must_use]
    pub fn shape(&self, id: TileId) -> Option<Shape> {
        self.bodies
            .iter()
            .find(|body| body.id == id)
            .map(Body::shape)
    }

    /// The tiles on their way out: their id, the row they last had, and how
    /// they are drawn now.
    #[must_use]
    pub fn leaving(&self) -> Vec<(TileId, usize, Shape)> {
        self.bodies
            .iter()
            .filter(|body| !body.wanted)
            .map(|body| (body.id, body.row, body.shape()))
            .collect()
    }

    /// Whether another frame is owed: a tile is arriving, moving or leaving.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        !self.is_settled()
    }

    fn is_settled(&self) -> bool {
        self.bodies.iter().all(Body::is_settled)
    }

    /// `cell` shrunk toward its centre to `level` of its size.
    #[must_use]
    pub fn cell(cell: Rect, level: u8) -> Rect {
        if level == u8::MAX {
            return cell;
        }
        let share = |whole: i32| (whole * i32::from(level) + 127) / 255;
        let (w, h) = (share(cell.w), share(cell.h));
        Rect::new(cell.x + (cell.w - w) / 2, cell.y + (cell.h - h) / 2, w, h)
    }

    /// How visible a tile's text is: none until the tile is mostly grown,
    /// then rising to full as it lands.
    #[must_use]
    pub fn text(level: u8) -> u8 {
        let above = u16::from(level).saturating_sub(Self::TEXT_FROM);
        let span = 255 - Self::TEXT_FROM;
        u8::try_from((above * 255 / span).min(255)).unwrap_or(u8::MAX)
    }
}

impl Default for TileGrowth {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Destination, Shape, TileGrowth};
    use crate::tile_id::TileId;
    use platform::draw::Rect;
    use std::path::Path;

    const HERE: &str = "/a";
    const BOUNDS: Rect = Rect::new(0, 0, 1000, 1000);

    fn dest(name: &str, x: i32) -> Destination {
        Destination {
            id: TileId::of(name),
            row: 0,
            rect: Rect::new(x, 0, 100, 100),
            slot: 0,
        }
    }

    fn names(count: usize) -> Vec<Destination> {
        (0..count)
            .map(|i| dest(&format!("n{i}"), i32::try_from(i).expect("small") * 100))
            .collect()
    }

    fn id(i: usize) -> TileId {
        TileId::of(&format!("n{i}"))
    }

    struct Clock(u64);

    impl Clock {
        fn run(&mut self, growth: &mut TileGrowth, targets: &[Destination], frames: u64) {
            for _ in 0..frames {
                self.0 += 16;
                growth.aim(Path::new(HERE), BOUNDS, targets, self.0);
                growth.advance(self.0);
            }
        }
    }

    fn landed(count: usize) -> (TileGrowth, Clock, Vec<Destination>) {
        let mut growth = TileGrowth::new();
        let mut clock = Clock(0);
        let targets = names(count);
        clock.run(&mut growth, &targets, 200);
        (growth, clock, targets)
    }

    #[test]
    fn before_any_scene_nothing_is_followed() {
        let growth = TileGrowth::new();
        assert!(!growth.is_primed());
        assert!(growth.shape(id(0)).is_none());
        assert!(!growth.is_animating());
    }

    #[test]
    fn a_new_scene_starts_with_nothing_grown() {
        let mut growth = TileGrowth::new();
        growth.aim(Path::new(HERE), BOUNDS, &names(6), 1000);
        assert_eq!(growth.shape(id(0)).expect("followed").grown, 0);
        assert_eq!(growth.shape(id(5)).expect("followed").grown, 0);
        assert!(growth.is_animating());
    }

    #[test]
    fn tiles_arrive_in_spiral_order() {
        let mut growth = TileGrowth::new();
        let mut clock = Clock(0);
        let targets = names(6);
        clock.run(&mut growth, &targets, 10);
        let levels: Vec<u8> = (0..6)
            .map(|i| growth.shape(id(i)).expect("followed").grown)
            .collect();
        assert!(levels[0] > 0, "{levels:?}");
        assert!(
            levels.windows(2).all(|pair| pair[0] >= pair[1]),
            "an earlier tile is never behind a later one: {levels:?}"
        );
        assert!(levels[0] > levels[5], "{levels:?}");
    }

    #[test]
    fn every_tile_lands_and_the_clock_stops() {
        let (growth, _clock, targets) = landed(8);
        for (i, dest) in targets.iter().enumerate() {
            assert_eq!(
                growth.shape(id(i)),
                Some(Shape::resting(dest.rect, dest.slot))
            );
        }
        assert!(!growth.is_animating());
    }

    #[test]
    fn the_whole_entrance_takes_bounded_time_however_many_tiles() {
        let mut growth = TileGrowth::new();
        let targets = names(60);
        let mut now = 0;
        loop {
            now += 16;
            growth.aim(Path::new(HERE), BOUNDS, &targets, now);
            growth.advance(now);
            if !growth.is_animating() {
                break;
            }
            assert!(now < 2_000, "still running at {now} ms");
        }
    }

    #[test]
    fn another_folder_grows_again_from_nothing() {
        let (mut growth, clock, targets) = landed(4);
        growth.aim(Path::new("/b"), BOUNDS, &targets, clock.0 + 100);
        assert_eq!(growth.shape(id(0)).expect("followed").grown, 0);
        assert!(growth.is_animating());
    }

    #[test]
    fn a_tile_found_later_grows_without_restarting_the_rest() {
        let (mut growth, mut clock, mut targets) = landed(3);
        targets.push(dest("found", 300));
        clock.run(&mut growth, &targets, 1);
        assert_eq!(growth.shape(id(0)).expect("followed").grown, 255);
        assert_eq!(growth.shape(TileId::of("found")).expect("new").grown, 0);
        clock.run(&mut growth, &targets, 30);
        assert_eq!(growth.shape(TileId::of("found")).expect("new").grown, 255);
    }

    #[test]
    fn a_tile_whose_cell_changed_glides_there_instead_of_jumping() {
        let (mut growth, mut clock, mut targets) = landed(3);
        let from = growth.shape(id(1)).expect("followed").rect;
        targets[1].rect = Rect::new(500, 300, 100, 100);
        clock.run(&mut growth, &targets, 1);
        let start = growth.shape(id(1)).expect("followed").rect;
        assert_eq!(start, from, "the first frame has not moved yet");
        clock.run(&mut growth, &targets, 8);
        let part_way = growth.shape(id(1)).expect("followed").rect;
        assert!(part_way.x > from.x && part_way.x < 500, "{part_way:?}");
        assert!(part_way.y > from.y && part_way.y < 300, "{part_way:?}");
        clock.run(&mut growth, &targets, 40);
        assert_eq!(
            growth.shape(id(1)).expect("followed").rect,
            Rect::new(500, 300, 100, 100)
        );
        assert!(!growth.is_animating());
    }

    #[test]
    fn a_glide_covers_a_tenth_of_the_way_in_its_first_frame_after_the_clock_runs() {
        let (mut growth, mut clock, mut targets) = landed(2);
        targets[0].rect = Rect::new(1000, 0, 100, 100);
        clock.run(&mut growth, &targets, 2);
        let moved = growth.shape(id(0)).expect("followed").rect.x;
        assert!(moved >= 100, "only {moved} of 1000 after one running frame");
    }

    #[test]
    fn repeated_retargets_keep_a_tile_moving_at_speed() {
        let (mut growth, mut clock, mut targets) = landed(2);
        let start = growth.shape(id(0)).expect("followed").rect.x;
        for step in 1..=6 {
            targets[0].rect = Rect::new(start + step * 100, 0, 100, 100);
            clock.run(&mut growth, &targets, 3);
        }
        let reached = growth.shape(id(0)).expect("followed").rect.x - start;
        assert!(reached >= 400, "covered {reached} of 600 in 288 ms");
    }

    #[test]
    fn a_retarget_midway_turns_around_from_where_the_tile_is() {
        let (mut growth, mut clock, mut targets) = landed(2);
        targets[0].rect = Rect::new(800, 0, 100, 100);
        clock.run(&mut growth, &targets, 8);
        let seen = growth.shape(id(0)).expect("followed").rect;
        targets[0].rect = Rect::new(0, 0, 100, 100);
        clock.run(&mut growth, &targets, 1);
        let after = growth.shape(id(0)).expect("followed").rect;
        assert!(
            (after.x - seen.x).abs() < 120,
            "no jump from {seen:?} to {after:?}"
        );
    }

    #[test]
    fn a_tile_that_is_no_longer_wanted_shrinks_away_and_is_forgotten() {
        let (mut growth, mut clock, targets) = landed(3);
        let kept = &targets[..2];
        clock.run(&mut growth, kept, 1);
        assert_eq!(growth.leaving().len(), 1);
        clock.run(&mut growth, kept, 5);
        let shrinking = growth.shape(id(2)).expect("still followed").grown;
        assert!(shrinking > 0 && shrinking < 255, "{shrinking}");
        clock.run(&mut growth, kept, 40);
        assert!(growth.shape(id(2)).is_none());
        assert!(growth.leaving().is_empty());
        assert!(!growth.is_animating());
    }

    #[test]
    fn a_tile_wanted_again_while_leaving_grows_back_from_where_it_is() {
        let (mut growth, mut clock, targets) = landed(3);
        clock.run(&mut growth, &targets[..2], 5);
        let shrinking = growth.shape(id(2)).expect("followed").grown;
        clock.run(&mut growth, &targets, 1);
        assert!(growth.leaving().is_empty());
        let back = growth.shape(id(2)).expect("followed").grown;
        assert!(
            back >= shrinking.saturating_sub(40),
            "{shrinking} -> {back}"
        );
        clock.run(&mut growth, &targets, 40);
        assert_eq!(growth.shape(id(2)).expect("followed").grown, 255);
    }

    #[test]
    fn settling_lands_every_glide_at_once() {
        let (mut growth, mut clock, mut targets) = landed(2);
        targets[0].rect = Rect::new(800, 0, 100, 100);
        clock.run(&mut growth, &targets, 2);
        growth.settle();
        assert_eq!(
            growth.shape(id(0)).expect("followed").rect,
            Rect::new(800, 0, 100, 100)
        );
    }

    #[test]
    fn a_moving_tile_blends_its_colour_slot_across_the_trip() {
        let (mut growth, mut clock, mut targets) = landed(2);
        targets[0].slot = 3;
        clock.run(&mut growth, &targets, 1);
        let start = growth.shape(id(0)).expect("followed");
        assert_eq!((start.from_slot, start.slot, start.mix), (0, 3, 0));
        clock.run(&mut growth, &targets, 40);
        let end = growth.shape(id(0)).expect("followed");
        assert_eq!((end.slot, end.mix), (3, 255));
    }

    #[test]
    fn resizing_the_field_moves_the_tiles_at_once() {
        let (mut growth, clock, mut targets) = landed(2);
        targets[0].rect = Rect::new(700, 0, 100, 100);
        growth.aim(
            Path::new(HERE),
            Rect::new(0, 0, 900, 1000),
            &targets,
            clock.0 + 16,
        );
        assert_eq!(
            growth.shape(id(0)).expect("followed").rect,
            Rect::new(700, 0, 100, 100)
        );
    }

    #[test]
    fn a_stalled_frame_resumes_instead_of_skipping_to_the_end() {
        let mut growth = TileGrowth::new();
        growth.aim(Path::new(HERE), BOUNDS, &names(1), 0);
        growth.advance(10_000);
        assert!(growth.shape(id(0)).expect("followed").grown < 255);
    }

    #[test]
    fn a_cell_shrinks_toward_its_centre() {
        let cell = Rect::new(100, 100, 200, 100);
        assert_eq!(TileGrowth::cell(cell, 255), cell);
        let half = TileGrowth::cell(cell, 128);
        assert_eq!((half.w, half.h), (100, 50));
        assert_eq!(half.x + half.w / 2, cell.x + cell.w / 2);
        assert_eq!(half.y + half.h / 2, cell.y + cell.h / 2);
        let none = TileGrowth::cell(cell, 0);
        assert_eq!((none.w, none.h), (0, 0));
    }

    #[test]
    fn text_waits_for_the_tile_to_be_mostly_grown() {
        assert_eq!(TileGrowth::text(0), 0);
        assert_eq!(TileGrowth::text(220), 0);
        assert_eq!(TileGrowth::text(255), 255);
        assert!(TileGrowth::text(240) > 0 && TileGrowth::text(240) < 255);
    }
}
