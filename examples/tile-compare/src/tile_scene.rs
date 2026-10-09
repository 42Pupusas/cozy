//! [`TileScene`] -- a spiral of tiles that shrink by rank, a quarter of
//! which change colour each frame.

use monitor::weighted_spiral::WeightedSpiral;
use platform::draw::{Color, Rect};
use platform::scene::SolidFrame;

pub struct TileScene {
    tiles: usize,
    frame: u64,
}

impl TileScene {
    const DECAY: f64 = 0.88;
    const GAP: i32 = 6;
    const LEVELS: u64 = 24;
    const BACKGROUND: Color = Color::rgb(0x16, 0x16, 0x1d);
    const HUES: [Color; 6] = [
        Color::rgb(0x7e, 0x9c, 0xd8),
        Color::rgb(0x98, 0xbb, 0x6c),
        Color::rgb(0xe6, 0xc3, 0x84),
        Color::rgb(0xd2, 0x7e, 0x99),
        Color::rgb(0x7f, 0xb4, 0xca),
        Color::rgb(0x95, 0x7f, 0xb8),
    ];

    #[must_use]
    pub const fn new(tiles: usize) -> Self {
        Self { tiles, frame: 0 }
    }

    pub const fn advance(&mut self) {
        self.frame += 1;
    }

    #[must_use]
    pub const fn background() -> Color {
        Self::BACKGROUND
    }

    #[must_use]
    pub fn frame(&self, width: i32, height: i32) -> SolidFrame {
        let area = Rect::new(
            Self::GAP,
            Self::GAP,
            width - 2 * Self::GAP,
            height - 2 * Self::GAP,
        );
        let weights: Vec<f64> = (0..self.tiles)
            .map(|i| Self::DECAY.powi(i32::try_from(i).unwrap_or(i32::MAX)))
            .collect();
        let mut frame = SolidFrame::new(Self::BACKGROUND);
        for (index, cell) in WeightedSpiral::new(area)
            .cells(&weights)
            .into_iter()
            .enumerate()
        {
            let tile = cell.inset(Self::GAP / 2);
            if tile.w > 0 && tile.h > 0 {
                frame.push(tile, self.color(index));
            }
        }
        frame
    }

    /// Tile `index` steps its brightness on every fourth frame, offset by its
    /// index, so a quarter of the tiles change on any frame.
    fn color(&self, index: usize) -> Color {
        let base = Self::HUES[index % Self::HUES.len()];
        let index = index as u64;
        let step = (self.frame + index) / 4 + index * 7;
        let level = step % (2 * Self::LEVELS);
        let level = if level >= Self::LEVELS {
            2 * Self::LEVELS - 1 - level
        } else {
            level
        };
        let scale = 96 + u32::try_from(level * 160 / Self::LEVELS).unwrap_or(0);
        let channel = |c: u8| u8::try_from(u32::from(c) * scale / 255).unwrap_or(u8::MAX);
        Color::rgb(channel(base.r), channel(base.g), channel(base.b))
    }
}

#[cfg(test)]
mod tests {
    use super::TileScene;

    #[test]
    fn every_tile_lands_inside_the_window() {
        let frame = TileScene::new(40).frame(1200, 800);
        assert_eq!(frame.rects.len(), 40);
        for tile in &frame.rects {
            assert!(tile.rect.x >= 0 && tile.rect.right() <= 1200);
            assert!(tile.rect.y >= 0 && tile.rect.bottom() <= 800);
        }
    }

    #[test]
    fn about_a_quarter_of_the_tiles_change_per_frame() {
        let mut scene = TileScene::new(40);
        let before = scene.frame(1200, 800);
        scene.advance();
        let after = scene.frame(1200, 800);
        let changed = before
            .rects
            .iter()
            .zip(&after.rects)
            .filter(|(a, b)| a.color != b.color)
            .count();
        assert!((8..=12).contains(&changed), "{changed} of 40 changed");
    }

    #[test]
    fn the_palette_stays_small_enough_to_share_buffers() {
        let mut scene = TileScene::new(40);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2_000 {
            for tile in scene.frame(1200, 800).rects {
                seen.insert((tile.color.r, tile.color.g, tile.color.b));
            }
            scene.advance();
        }
        assert!(seen.len() <= 6 * 24, "{} colours", seen.len());
    }
}
