//! [`TileGrowth`] -- process tiles growing out of their cells and gliding
//! between them.
//!
//! Every tile is followed by its key, not its position. A tile met for the
//! first time grows from the centre of its cell; one whose cell changed
//! glides from where it was drawn to the new cell; one no longer wanted
//! shrinks away where it stood. Another scene (a group opened or closed)
//! starts the field over, its tiles arriving one after another in rank order.

use platform::draw::Rect;

use crate::fade::Fade;
use crate::tile_key::TileKey;

/// Where the layout wants a tile to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    pub key: TileKey,
    pub rect: Rect,
}

/// How a tile is drawn this frame: the cell it occupies right now and how
/// much of that cell it has grown to fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub rect: Rect,
    pub grown: u8,
}

impl Shape {
    #[must_use]
    pub const fn resting(rect: Rect) -> Self {
        Self {
            rect,
            grown: u8::MAX,
        }
    }
}

struct Body {
    key: TileKey,
    from: Rect,
    to: Rect,
    travel: Fade,
    grown: Fade,
    wanted: bool,
    start_ms: u64,
}

impl Body {
    const GROW_MS: u16 = 280;
    const MOVE_MS: u16 = 260;

    fn arriving(dest: &Destination, start_ms: u64) -> Self {
        Self {
            key: dest.key.clone(),
            from: dest.rect,
            to: dest.rect,
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
        Shape {
            rect: self.rect(),
            grown: self.grown.level(),
        }
    }

    fn follow(&mut self, dest: &Destination) {
        if !self.wanted {
            self.wanted = true;
            self.grown.aim(true);
        }
        if self.to != dest.rect {
            self.from = self.rect();
            self.to = dest.rect;
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
            self.grown.step(elapsed_ms, Self::GROW_MS);
        }
        self.travel.step(elapsed_ms, Self::MOVE_MS);
    }

    const fn settle(&mut self) {
        self.travel = Fade::full();
        self.from = self.to;
    }

    fn is_settled(&self) -> bool {
        self.grown.is_settled() && self.travel.is_settled()
    }

    fn is_gone(&self) -> bool {
        !self.wanted && self.grown.is_gone()
    }
}

/// Which set of tiles is on screen: none yet, the top-level groups, or one
/// group's members.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum GrowthScene {
    #[default]
    Unset,
    Groups,
    Members(String),
}

impl GrowthScene {
    fn of(group: Option<&str>) -> Self {
        group.map_or(Self::Groups, |name| Self::Members(name.to_owned()))
    }
}

#[derive(Default)]
pub struct TileGrowth {
    scene: GrowthScene,
    bounds: Option<Rect>,
    bodies: Vec<Body>,
    clock: Option<u64>,
}

impl TileGrowth {
    const MAX_STEP_MS: u64 = 64;
    const STAGGER_MS: u64 = 55;
    const LONGEST_STAGGER_MS: u64 = 500;
    const TEXT_FROM: u16 = 220;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the tiles at `targets`, in rank order, for the `scene` (the
    /// expanded group, if any) laid out inside `bounds`. A change of `bounds`
    /// lands every tile at once rather than trailing a dragged edge.
    pub fn aim(&mut self, scene: Option<&str>, bounds: Rect, targets: &[Destination], now_ms: u64) {
        let scene = GrowthScene::of(scene);
        let entrance = self.scene != scene;
        if entrance {
            self.bodies.clear();
            self.clock = None;
            self.scene = scene;
        }
        let resized = self.bounds.is_some_and(|seen| seen != bounds);
        self.bounds = Some(bounds);
        for body in &mut self.bodies {
            if body.wanted && !targets.iter().any(|dest| dest.key == body.key) {
                body.release();
            }
        }
        let gap = Self::gap_ms(targets.len());
        for (index, dest) in targets.iter().enumerate() {
            if let Some(body) = self.bodies.iter_mut().find(|body| body.key == dest.key) {
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
            self.bodies.retain(|body| body.wanted);
            self.bodies.iter_mut().for_each(Body::settle);
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

    /// How the tile `key` is drawn, when it is being followed.
    #[must_use]
    pub fn shape(&self, key: &TileKey) -> Option<Shape> {
        self.bodies
            .iter()
            .find(|body| body.key == *key)
            .map(Body::shape)
    }

    /// The tiles on their way out and how they are drawn now.
    #[must_use]
    pub fn leaving(&self) -> Vec<(TileKey, Shape)> {
        self.bodies
            .iter()
            .filter(|body| !body.wanted)
            .map(|body| (body.key.clone(), body.shape()))
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

#[cfg(test)]
mod tests {
    use super::{Destination, Shape, TileGrowth};
    use crate::tile_key::TileKey;
    use platform::draw::Rect;

    const BOUNDS: Rect = Rect::new(0, 0, 1000, 1000);

    fn key(i: usize) -> TileKey {
        TileKey::Process(format!("n{i}"))
    }

    fn names(count: usize) -> Vec<Destination> {
        (0..count)
            .map(|i| Destination {
                key: key(i),
                rect: Rect::new(i32::try_from(i).expect("small") * 100, 0, 100, 100),
            })
            .collect()
    }

    struct Clock(u64);

    impl Clock {
        fn run(&mut self, growth: &mut TileGrowth, targets: &[Destination], frames: u64) {
            for _ in 0..frames {
                self.0 += 16;
                growth.aim(None, BOUNDS, targets, self.0);
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
    fn a_new_scene_starts_with_nothing_grown() {
        let mut growth = TileGrowth::new();
        growth.aim(None, BOUNDS, &names(6), 1000);
        assert_eq!(growth.shape(&key(0)).expect("followed").grown, 0);
        assert!(growth.is_animating());
    }

    #[test]
    fn tiles_arrive_in_rank_order() {
        let mut growth = TileGrowth::new();
        let mut clock = Clock(0);
        let targets = names(6);
        clock.run(&mut growth, &targets, 10);
        let levels: Vec<u8> = (0..6)
            .map(|i| growth.shape(&key(i)).expect("followed").grown)
            .collect();
        assert!(levels[0] > levels[5], "{levels:?}");
        assert!(
            levels.windows(2).all(|pair| pair[0] >= pair[1]),
            "{levels:?}"
        );
    }

    #[test]
    fn every_tile_lands_and_the_clock_stops() {
        let (growth, _clock, targets) = landed(8);
        for (i, dest) in targets.iter().enumerate() {
            assert_eq!(growth.shape(&key(i)), Some(Shape::resting(dest.rect)));
        }
        assert!(!growth.is_animating());
    }

    #[test]
    fn the_whole_entrance_takes_bounded_time_however_many_tiles() {
        let mut growth = TileGrowth::new();
        let mut clock = Clock(0);
        let targets = names(120);
        clock.run(&mut growth, &targets, 1);
        while growth.is_animating() {
            clock.run(&mut growth, &targets, 1);
            assert!(clock.0 < 2_000, "still running at {} ms", clock.0);
        }
    }

    #[test]
    fn opening_a_group_grows_again_from_nothing() {
        let (mut growth, clock, targets) = landed(4);
        growth.aim(Some("helium"), BOUNDS, &targets, clock.0 + 16);
        assert_eq!(growth.shape(&key(0)).expect("followed").grown, 0);
    }

    #[test]
    fn a_tile_whose_cell_changed_glides_there() {
        let (mut growth, mut clock, mut targets) = landed(3);
        let from = growth.shape(&key(1)).expect("followed").rect;
        targets[1].rect = Rect::new(500, 300, 100, 100);
        clock.run(&mut growth, &targets, 1);
        assert_eq!(growth.shape(&key(1)).expect("followed").rect, from);
        clock.run(&mut growth, &targets, 6);
        let part = growth.shape(&key(1)).expect("followed").rect;
        assert!(part.x > from.x && part.x < 500, "{part:?}");
        clock.run(&mut growth, &targets, 40);
        assert_eq!(
            growth.shape(&key(1)).expect("followed").rect,
            Rect::new(500, 300, 100, 100)
        );
    }

    #[test]
    fn an_unwanted_tile_shrinks_away_and_is_forgotten() {
        let (mut growth, mut clock, targets) = landed(3);
        clock.run(&mut growth, &targets[..2], 5);
        assert_eq!(growth.leaving().len(), 1);
        let shrinking = growth.shape(&key(2)).expect("followed").grown;
        assert!(shrinking > 0 && shrinking < 255);
        clock.run(&mut growth, &targets[..2], 40);
        assert!(growth.shape(&key(2)).is_none());
        assert!(!growth.is_animating());
    }

    #[test]
    fn a_new_tile_in_the_same_scene_grows_without_restarting_the_rest() {
        let (mut growth, mut clock, mut targets) = landed(3);
        targets.push(Destination {
            key: TileKey::More,
            rect: Rect::new(300, 0, 100, 100),
        });
        clock.run(&mut growth, &targets, 1);
        assert_eq!(growth.shape(&key(0)).expect("followed").grown, 255);
        assert!(growth.shape(&TileKey::More).expect("new").grown < 255);
    }

    #[test]
    fn resizing_lands_every_tile_at_once_and_drops_the_leaving() {
        let (mut growth, clock, mut targets) = landed(3);
        targets[0].rect = Rect::new(700, 0, 100, 100);
        growth.aim(None, Rect::new(0, 0, 900, 900), &targets[..2], clock.0 + 16);
        assert_eq!(
            growth.shape(&key(0)).expect("followed").rect,
            Rect::new(700, 0, 100, 100)
        );
        assert!(growth.leaving().is_empty());
    }

    #[test]
    fn a_cell_shrinks_toward_its_centre_and_text_waits() {
        let cell = Rect::new(100, 100, 200, 100);
        assert_eq!(TileGrowth::cell(cell, 255), cell);
        let half = TileGrowth::cell(cell, 128);
        assert_eq!((half.w, half.h), (100, 50));
        assert_eq!(half.x + half.w / 2, cell.x + cell.w / 2);
        assert_eq!(TileGrowth::text(200), 0);
        assert_eq!(TileGrowth::text(255), 255);
    }
}
