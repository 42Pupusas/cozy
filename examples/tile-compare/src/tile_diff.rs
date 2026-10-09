//! [`TileDiff`] -- the rects that differ between two frames of the same
//! scene, as buffer damage.

use platform::draw::{Damage, Rect};
use platform::scene::SolidFrame;

pub struct TileDiff;

impl TileDiff {
    /// Every tile whose rect or colour changed, plus the old rect of a moved
    /// tile. A change in tile count or background damages everything.
    #[must_use]
    pub fn damage(before: &SolidFrame, after: &SolidFrame) -> Damage {
        if before.background != after.background || before.rects.len() != after.rects.len() {
            return Damage::full();
        }
        let mut rects: Vec<Rect> = Vec::new();
        for (old, new) in before.rects.iter().zip(&after.rects) {
            if old.rect != new.rect {
                rects.push(old.rect);
                rects.push(new.rect);
            } else if old.color != new.color {
                rects.push(new.rect);
            }
        }
        Damage::regions(rects)
    }
}

#[cfg(test)]
mod tests {
    use super::TileDiff;
    use platform::draw::{Color, Rect};
    use platform::scene::SolidFrame;

    const BG: Color = Color::rgb(0, 0, 0);
    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    fn frame(tiles: &[(Rect, Color)]) -> SolidFrame {
        let mut frame = SolidFrame::new(BG);
        for &(rect, color) in tiles {
            frame.push(rect, color);
        }
        frame
    }

    #[test]
    fn an_unchanged_frame_damages_nothing() {
        let a = frame(&[(Rect::new(0, 0, 10, 10), RED)]);
        assert!(TileDiff::damage(&a, &a.clone()).is_empty());
    }

    #[test]
    fn a_recolour_damages_only_that_tile() {
        let a = frame(&[
            (Rect::new(0, 0, 10, 10), RED),
            (Rect::new(20, 0, 5, 5), RED),
        ]);
        let b = frame(&[
            (Rect::new(0, 0, 10, 10), RED),
            (Rect::new(20, 0, 5, 5), BLUE),
        ]);
        assert_eq!(TileDiff::damage(&a, &b).rects(), &[Rect::new(20, 0, 5, 5)]);
    }

    #[test]
    fn a_move_damages_where_it_was_and_where_it_is() {
        let a = frame(&[(Rect::new(0, 0, 10, 10), RED)]);
        let b = frame(&[(Rect::new(5, 0, 10, 10), RED)]);
        assert_eq!(
            TileDiff::damage(&a, &b).rects(),
            &[Rect::new(0, 0, 10, 10), Rect::new(5, 0, 10, 10)]
        );
    }

    #[test]
    fn a_different_tile_count_damages_everything() {
        let a = frame(&[(Rect::new(0, 0, 10, 10), RED)]);
        let b = frame(&[]);
        assert!(TileDiff::damage(&a, &b).is_full());
    }
}
