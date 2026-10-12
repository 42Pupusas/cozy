//! [`TileText`] -- picks a tile's text sizes from the tile's size.
//!
//! The name takes the largest ladder step whose line is at most a quarter of
//! the tile's height, fits across it, and leaves room for the detail lines.

use crate::text_ladder::TextLadder;

/// The measurements [`TileText`] needs from a ladder of text sizes.
pub trait LadderMetrics {
    fn steps(&self) -> usize;
    fn line_height(&self, step: usize) -> i32;
    fn width(&mut self, step: usize, text: &str) -> i32;
}

impl LadderMetrics for TextLadder {
    fn steps(&self) -> usize {
        self.len()
    }

    fn line_height(&self, step: usize) -> i32 {
        Self::line_height(self, step)
    }

    fn width(&mut self, step: usize, text: &str) -> i32 {
        self.step(step).measure(text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileText {
    pub name_step: usize,
    pub detail_step: usize,
}

impl TileText {
    const HEIGHT_SHARE: i32 = 4;
    const DETAIL_DROP: usize = 2;

    #[must_use]
    pub fn fit(
        ladder: &mut impl LadderMetrics,
        name: &str,
        width: i32,
        height: i32,
        details: usize,
    ) -> Self {
        let tallest = height / Self::HEIGHT_SHARE;
        let lines = i32::try_from(details).unwrap_or(i32::MAX);
        (0..ladder.steps())
            .rev()
            .map(Self::at)
            .find(|text| {
                let name_line = ladder.line_height(text.name_step);
                let detail_line = ladder.line_height(text.detail_step);
                name_line <= tallest
                    && name_line + detail_line.saturating_mul(lines) <= height
                    && ladder.width(text.name_step, name) <= width
            })
            .unwrap_or_else(|| Self::at(0))
    }

    const fn at(name_step: usize) -> Self {
        Self {
            name_step,
            detail_step: name_step.saturating_sub(Self::DETAIL_DROP),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LadderMetrics, TileText};

    struct Fake;

    impl Fake {
        const LINES: [i32; 8] = [14, 17, 21, 27, 33, 42, 52, 65];
    }

    impl LadderMetrics for Fake {
        fn steps(&self) -> usize {
            Self::LINES.len()
        }

        fn line_height(&self, step: usize) -> i32 {
            Self::LINES[step]
        }

        fn width(&mut self, step: usize, text: &str) -> i32 {
            let chars = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
            chars * Self::LINES[step] / 2
        }
    }

    #[test]
    fn a_larger_tile_gets_larger_text() {
        let small = TileText::fit(&mut Fake, "helium", 120, 70, 0);
        let large = TileText::fit(&mut Fake, "helium", 700, 420, 0);
        assert!(large.name_step > small.name_step, "{small:?} {large:?}");
        assert!(Fake.line_height(large.name_step) <= 420 / 4);
    }

    #[test]
    fn a_long_name_shrinks_to_fit_the_width() {
        let name = "a-rather-long-process-name";
        let short = TileText::fit(&mut Fake, "sh", 300, 400, 0);
        let long = TileText::fit(&mut Fake, name, 300, 400, 0);
        assert!(long.name_step < short.name_step);
        assert!(Fake.width(long.name_step, name) <= 300);
    }

    #[test]
    fn a_large_tile_keeps_its_name_size_when_details_appear() {
        let quiet = TileText::fit(&mut Fake, "helium", 700, 420, 0);
        let hovered = TileText::fit(&mut Fake, "helium", 700, 420, 2);
        assert_eq!(quiet, hovered);
    }

    #[test]
    fn a_short_tile_shrinks_its_name_to_make_room_for_details() {
        let height = 108;
        let quiet = TileText::fit(&mut Fake, "x", 600, height, 0);
        let hovered = TileText::fit(&mut Fake, "x", 600, height, 5);
        assert!(hovered.name_step < quiet.name_step, "{quiet:?} {hovered:?}");
        let used = Fake.line_height(hovered.name_step) + Fake.line_height(hovered.detail_step) * 5;
        assert!(used <= height);
    }

    #[test]
    fn details_sit_below_the_name_size() {
        let text = TileText::fit(&mut Fake, "x", 900, 900, 0);
        assert!(text.detail_step < text.name_step);
        let none = TileText::fit(&mut Fake, "x", 1, 1, 0);
        assert_eq!((none.name_step, none.detail_step), (0, 0));
    }
}
