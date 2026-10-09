//! [`TileGrowth`] -- tiles arriving one after another along the spiral, each
//! easing up from nothing to its cell.
//!
//! A new scene (another folder, another overflow page) restarts the
//! entrance. Within a scene a tile that appears later, such as a file that
//! was just created, grows alone. Before the first scene is seen every tile
//! counts as fully grown, so a frame painted without a running clock still
//! shows the whole field.

use std::path::PathBuf;

use platform::draw::Rect;

use crate::fade::Fades;
use crate::tile_motion::TileKey;

/// What the field is showing: the folder and how many overflow pages deep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrowthScene {
    pub path: PathBuf,
    pub depth: usize,
}

pub struct TileGrowth {
    scene: Option<GrowthScene>,
    began_ms: u64,
    fades: Fades<TileKey>,
    waiting: bool,
}

impl TileGrowth {
    const GROW_MS: u16 = 280;
    const STAGGER_MS: u64 = 55;
    const LONGEST_STAGGER_MS: u64 = 500;
    const TEXT_FROM: u16 = 220;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            scene: None,
            began_ms: 0,
            fades: Fades::lasting(Self::GROW_MS),
            waiting: false,
        }
    }

    /// Reveal `order`, the field's tiles in spiral order, as time passes in
    /// `scene`.
    pub fn aim(&mut self, scene: &GrowthScene, order: &[TileKey], now_ms: u64) {
        if self.scene.as_ref() != Some(scene) {
            self.fades.clear();
            self.scene = Some(scene.clone());
            self.began_ms = now_ms;
        }
        let gap = Self::gap_ms(order.len());
        let elapsed = now_ms.saturating_sub(self.began_ms);
        let due = usize::try_from(1 + elapsed / gap).unwrap_or(usize::MAX);
        let shown = due.min(order.len());
        self.waiting = shown < order.len();
        self.fades.aim(&order[..shown], now_ms);
    }

    fn gap_ms(count: usize) -> u64 {
        let count = u64::try_from(count).unwrap_or(u64::MAX).max(1);
        Self::STAGGER_MS
            .min(Self::LONGEST_STAGGER_MS / count)
            .max(1)
    }

    pub fn advance(&mut self, now_ms: u64) {
        self.fades.advance(now_ms);
    }

    /// How grown the tile is, 0 to 255.
    #[must_use]
    pub fn level(&self, key: TileKey) -> u8 {
        if self.scene.is_none() {
            return u8::MAX;
        }
        self.fades.level(&key)
    }

    /// Whether another frame is owed: a tile is growing, or one is still
    /// waiting for its turn.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.waiting || self.fades.is_animating()
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
    use super::{GrowthScene, TileGrowth};
    use crate::tile_motion::TileKey;
    use platform::draw::Rect;
    use std::path::PathBuf;

    fn scene(path: &str) -> GrowthScene {
        GrowthScene {
            path: PathBuf::from(path),
            depth: 0,
        }
    }

    fn keys(count: usize) -> Vec<TileKey> {
        (0..count).map(TileKey::Item).collect()
    }

    fn run(growth: &mut TileGrowth, from_ms: u64, frames: u64) {
        for frame in 1..=frames {
            growth.advance(from_ms + frame * 16);
        }
    }

    #[test]
    fn before_any_scene_every_tile_is_fully_grown() {
        let growth = TileGrowth::new();
        assert_eq!(growth.level(TileKey::Item(3)), 255);
        assert!(!growth.is_animating());
    }

    #[test]
    fn a_new_scene_starts_with_nothing_grown() {
        let mut growth = TileGrowth::new();
        growth.aim(&scene("/a"), &keys(6), 1000);
        assert_eq!(growth.level(TileKey::Item(0)), 0);
        assert_eq!(growth.level(TileKey::Item(5)), 0);
        assert!(growth.is_animating());
    }

    #[test]
    fn tiles_arrive_in_spiral_order() {
        let mut growth = TileGrowth::new();
        let order = keys(6);
        growth.aim(&scene("/a"), &order, 0);
        for frame in 1..=10 {
            let now = frame * 16;
            growth.aim(&scene("/a"), &order, now);
            growth.advance(now);
        }
        let levels: Vec<u8> = order.iter().map(|&key| growth.level(key)).collect();
        assert!(levels[0] > 0, "{levels:?}");
        assert!(
            levels.windows(2).all(|pair| pair[0] >= pair[1]),
            "an earlier tile is never behind a later one: {levels:?}"
        );
        assert!(levels[0] > levels[5], "{levels:?}");
    }

    #[test]
    fn every_tile_lands_and_the_clock_stops() {
        let mut growth = TileGrowth::new();
        let order = keys(8);
        for frame in 0..200 {
            let now = frame * 16;
            growth.aim(&scene("/a"), &order, now);
            growth.advance(now);
        }
        assert!(order.iter().all(|&key| growth.level(key) == 255));
        assert!(!growth.is_animating());
    }

    #[test]
    fn another_folder_grows_again_from_nothing() {
        let mut growth = TileGrowth::new();
        let order = keys(4);
        for frame in 0..200 {
            let now = frame * 16;
            growth.aim(&scene("/a"), &order, now);
            growth.advance(now);
        }
        growth.aim(&scene("/b"), &order, 4000);
        assert_eq!(growth.level(TileKey::Item(0)), 0);
        assert!(growth.is_animating());
    }

    #[test]
    fn a_tile_that_appears_later_grows_without_restarting_the_rest() {
        let mut growth = TileGrowth::new();
        for frame in 0..200 {
            let now = frame * 16;
            growth.aim(&scene("/a"), &keys(3), now);
            growth.advance(now);
        }
        growth.aim(&scene("/a"), &keys(4), 4000);
        assert_eq!(growth.level(TileKey::Item(0)), 255);
        assert_eq!(growth.level(TileKey::Item(3)), 0);
        run(&mut growth, 4000, 30);
        assert_eq!(growth.level(TileKey::Item(3)), 255);
    }

    #[test]
    fn the_whole_entrance_takes_bounded_time_however_many_tiles() {
        let mut growth = TileGrowth::new();
        let order = keys(60);
        let mut now = 0;
        while growth.is_animating() || now == 0 {
            growth.aim(&scene("/a"), &order, now);
            growth.advance(now);
            now += 16;
            assert!(now < 2_000, "still running at {now} ms");
        }
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
