//! [`TileAction`] and [`ActionBar`] -- the buttons a hovered tile offers, and
//! where they sit on it.

use platform::draw::{Point, Rect};

use crate::spacing::Spacing;

/// What a tile's buttons can do to the entry under them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileAction {
    CopyPath,
    Copy,
    Cut,
    Rename,
}

impl TileAction {
    pub const ALL: [Self; 4] = [Self::CopyPath, Self::Copy, Self::Cut, Self::Rename];

    /// Position in [`Self::ALL`], for arrays indexed by action.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::CopyPath => 0,
            Self::Copy => 1,
            Self::Cut => 2,
            Self::Rename => 3,
        }
    }
}

/// The buttons laid out on one tile face.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActionBar {
    buttons: Vec<(TileAction, Rect)>,
}

impl ActionBar {
    /// Centre a row of buttons on `face`, below the name and size (which end
    /// at `text_bottom`).
    ///
    /// A face too small to hold every button at the least readable size gets
    /// none: a bar with some actions missing would teach the user that the
    /// set varies.
    #[must_use]
    pub fn fit(face: Rect, text_bottom: i32, spacing: Spacing) -> Self {
        let count = i32::try_from(TileAction::ALL.len()).unwrap_or(1);
        let gap = spacing.xs();
        let margin = spacing.s();
        let across = (face.w - margin * 2 - gap * (count - 1)) / count;
        let below = face.bottom() - margin - (text_bottom + gap);
        let side = across.min(below).min(spacing.action_button());
        if side < spacing.action_button_min() {
            return Self::default();
        }
        let total = side * count + gap * (count - 1);
        let left = face.x + (face.w - total) / 2;
        let centred = face.y + (face.h - side) / 2;
        let top = centred
            .max(text_bottom + gap)
            .min(face.bottom() - margin - side);
        let buttons = TileAction::ALL
            .into_iter()
            .zip(0..)
            .map(|(action, i)| (action, Rect::new(left + i * (side + gap), top, side, side)))
            .collect();
        Self { buttons }
    }

    #[must_use]
    pub fn buttons(&self) -> &[(TileAction, Rect)] {
        &self.buttons
    }

    #[must_use]
    pub fn hit(&self, at: Point) -> Option<TileAction> {
        self.buttons
            .iter()
            .find(|(_, rect)| rect.contains(at))
            .map(|(action, _)| *action)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionBar, TileAction};
    use crate::spacing::Spacing;
    use platform::draw::{Point, Rect};

    fn spacing() -> Spacing {
        Spacing::new(13.0, 1.0)
    }

    fn roomy() -> Rect {
        Rect::new(0, 0, 220, 140)
    }

    #[test]
    fn an_actions_index_is_its_place_in_the_list() {
        for (place, action) in TileAction::ALL.into_iter().enumerate() {
            assert_eq!(action.index(), place);
        }
    }

    #[test]
    fn a_roomy_face_gets_every_action_in_order() {
        let bar = ActionBar::fit(roomy(), 40, spacing());
        let order: Vec<TileAction> = bar.buttons().iter().map(|(a, _)| *a).collect();
        assert_eq!(order, TileAction::ALL);
    }

    #[test]
    fn the_row_is_centred_across_the_face() {
        let face = roomy();
        let bar = ActionBar::fit(face, 40, spacing());
        let first = bar.buttons().first().expect("a button").1;
        let last = bar.buttons().last().expect("a button").1;
        let left = first.x - face.x;
        let right = face.right() - last.right();
        assert!((left - right).abs() <= 1, "left {left} vs right {right}");
    }

    #[test]
    fn buttons_stay_clear_of_the_text_and_inside_the_face() {
        for height in [96, 104, 140, 300] {
            let face = Rect::new(10, 10, 220, height);
            let bar = ActionBar::fit(face, 10 + 42, spacing());
            for (_, rect) in bar.buttons() {
                assert!(rect.y > 10 + 42, "overlaps the text at height {height}");
                assert!(rect.bottom() <= face.bottom(), "spills at height {height}");
                assert!(rect.x >= face.x && rect.right() <= face.right());
            }
        }
    }

    #[test]
    fn buttons_neither_overlap_nor_exceed_their_size() {
        let s = spacing();
        let bar = ActionBar::fit(roomy(), 40, s);
        for pair in bar.buttons().windows(2) {
            assert!(pair[0].1.right() < pair[1].1.x, "adjacent buttons touch");
        }
        for (_, rect) in bar.buttons() {
            assert!(rect.w <= s.action_button() && rect.w == rect.h);
        }
    }

    #[test]
    fn a_face_too_narrow_or_too_short_gets_none() {
        assert!(
            ActionBar::fit(Rect::new(0, 0, 60, 140), 40, spacing())
                .buttons()
                .is_empty()
        );
        assert!(
            ActionBar::fit(Rect::new(0, 0, 220, 60), 40, spacing())
                .buttons()
                .is_empty()
        );
    }

    #[test]
    fn a_hit_names_the_button_under_the_point() {
        let bar = ActionBar::fit(roomy(), 40, spacing());
        for (action, rect) in bar.buttons() {
            let centre = Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2);
            assert_eq!(bar.hit(centre), Some(*action));
        }
        assert_eq!(bar.hit(Point::new(1, 1)), None);
    }

    #[test]
    fn the_gap_between_buttons_hits_nothing() {
        let bar = ActionBar::fit(roomy(), 40, spacing());
        let a = bar.buttons()[0].1;
        let b = bar.buttons()[1].1;
        assert_eq!(bar.hit(Point::new(a.right(), a.y + 1)), None);
        assert!(a.right() < b.x);
    }

    #[test]
    fn a_larger_font_grows_the_buttons() {
        let small = ActionBar::fit(Rect::new(0, 0, 400, 300), 60, Spacing::new(13.0, 1.0));
        let big = ActionBar::fit(Rect::new(0, 0, 400, 300), 60, Spacing::new(26.0, 1.0));
        assert!(big.buttons()[0].1.w > small.buttons()[0].1.w);
    }
}
