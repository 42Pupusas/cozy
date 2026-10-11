//! [`ActionTray`]: the extra rows a right click adds to a tile.
//!
//! The menu actions sit in two rows of three, directly under the hover
//! buttons and in the same columns, with a caption naming the one pointed at.
//! They belong to the tile: its card grows to hold them.

use platform::draw::{Point, Rect};

use crate::spacing::Spacing;
use crate::tile_action::{ActionBar, TileAction};

const ACROSS: usize = 3;

/// Which tile a tray is open on, and the listing it was opened on, so a
/// re-read that moves the rows closes it instead of leaving it on the wrong
/// entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayOpen {
    pub row: usize,
    pub listing: u64,
}

/// Everything in resting coordinates, as for a tile that has not risen. The
/// drawn position is the resting one moved by [`Self::offset`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionTray {
    face: Rect,
    extra: i32,
    slide: i32,
    rows: Vec<(TileAction, Rect)>,
    caption: Rect,
}

impl ActionTray {
    /// Lay the rows out under the buttons of `bar` on the tile whose face is
    /// `face`, inside `field`.
    ///
    /// The card grows by only what the rows need beyond the face. If that
    /// would run past the bottom of the field, the whole card slides up by
    /// the overshoot instead, so the rows stay below the buttons. A face with
    /// no buttons gets no tray.
    #[must_use]
    pub fn fit(
        bar: &ActionBar,
        face: Rect,
        field: Rect,
        spacing: Spacing,
        caption_height: i32,
    ) -> Option<Self> {
        let columns = bar.buttons();
        if columns.len() != ACROSS {
            return None;
        }
        let first = columns[0].1;
        let last = columns[ACROSS - 1].1;
        let side = first.w;
        let gap = columns[1].1.x - first.right();
        let rows: Vec<(TileAction, Rect)> = TileAction::MENU
            .into_iter()
            .enumerate()
            .map(|(i, action)| {
                let line = i32::try_from(i / ACROSS).unwrap_or(0) + 1;
                let at = Rect::new(
                    columns[i % ACROSS].1.x,
                    first.y + line * (side + gap),
                    side,
                    side,
                );
                (action, at)
            })
            .collect();
        let grid_bottom = rows.last()?.1.bottom();
        let caption = Rect::new(
            first.x,
            grid_bottom + gap,
            last.right() - first.x,
            caption_height,
        );
        let bottom = caption.bottom() + spacing.m();
        let extra = (bottom - face.bottom()).max(0);
        let overshoot = face.bottom() + extra - field.bottom();
        let raised_by = overshoot.clamp(0, (face.y - field.y).max(0));
        Some(Self {
            face,
            extra,
            slide: raised_by,
            rows,
            caption,
        })
    }

    #[must_use]
    pub fn rows(&self) -> &[(TileAction, Rect)] {
        &self.rows
    }

    #[must_use]
    pub const fn caption(&self) -> Rect {
        self.caption
    }

    /// How far the card has slid up when the tray is `level` open.
    #[must_use]
    pub fn slide_at(&self, level: u8) -> i32 {
        Self::share(self.slide, level)
    }

    /// How much taller than its face the card is when the tray is `level`
    /// open.
    #[must_use]
    pub fn extra_at(&self, level: u8) -> i32 {
        Self::share(self.extra, level)
    }

    /// How far everything on the tile sits from its resting place: up by the
    /// tile's `lift` and by the slide so far.
    #[must_use]
    pub fn offset(&self, lift: i32, level: u8) -> i32 {
        -(lift + self.slide_at(level))
    }

    /// The tile's card as drawn.
    #[must_use]
    pub fn card(&self, lift: i32, level: u8) -> Rect {
        Rect::new(
            self.face.x,
            self.face.y + self.offset(lift, level),
            self.face.w,
            self.face.h + self.extra_at(level),
        )
    }

    /// The row button under `at`, where it is drawn.
    #[must_use]
    pub fn hit(&self, at: Point, lift: i32, level: u8) -> Option<TileAction> {
        let resting = Point::new(at.x, at.y - self.offset(lift, level));
        self.rows
            .iter()
            .find(|(_, rect)| rect.contains(resting))
            .map(|(action, _)| *action)
    }

    fn share(whole: i32, level: u8) -> i32 {
        (whole * i32::from(level) + 127) / 255
    }
}

#[cfg(test)]
mod tests {
    use super::ActionTray;
    use crate::spacing::Spacing;
    use crate::tile_action::{ActionBar, TileAction};
    use platform::draw::{Point, Rect};

    const FIELD: Rect = Rect::new(0, 0, 1000, 700);
    const CAPTION: i32 = 15;
    const TEXT_BOTTOM: i32 = 40;

    fn spacing() -> Spacing {
        Spacing::new(13.0, 1.0)
    }

    fn face_at(x: i32, y: i32) -> Rect {
        Rect::new(x, y, 200, 140)
    }

    fn bar(face: Rect) -> ActionBar {
        ActionBar::fit(face, face.y + TEXT_BOTTOM, spacing())
    }

    fn tray(face: Rect) -> ActionTray {
        ActionTray::fit(&bar(face), face, FIELD, spacing(), CAPTION).expect("room for buttons")
    }

    #[test]
    fn the_menu_actions_fill_two_rows_of_three_in_order() {
        let tray = tray(face_at(300, 100));
        let order: Vec<TileAction> = tray.rows().iter().map(|(a, _)| *a).collect();
        assert_eq!(order, TileAction::MENU);
        let ys: Vec<i32> = tray.rows().iter().map(|(_, r)| r.y).collect();
        assert!(ys[0] == ys[1] && ys[1] == ys[2]);
        assert!(ys[3] == ys[4] && ys[4] == ys[5]);
        assert!(ys[3] > ys[0]);
    }

    #[test]
    fn the_rows_sit_directly_under_the_hover_buttons_in_the_same_columns() {
        let face = face_at(300, 100);
        let bar = bar(face);
        let tray = tray(face);
        let primary = bar.buttons();
        let gap = primary[1].1.x - primary[0].1.right();
        for (i, (_, rect)) in tray.rows().iter().enumerate() {
            assert_eq!(rect.x, primary[i % 3].1.x);
            assert_eq!(rect.w, primary[0].1.w);
        }
        assert_eq!(tray.rows()[0].1.y, primary[0].1.bottom() + gap);
        assert_eq!(
            tray.rows()[3].1.y,
            tray.rows()[0].1.bottom() + gap,
            "and the rows are as far apart as the buttons are from them"
        );
    }

    #[test]
    fn the_caption_is_under_the_rows_and_the_card_holds_it() {
        let face = face_at(300, 100);
        let tray = tray(face);
        let rows_bottom = tray.rows().last().expect("rows").1.bottom();
        assert!(tray.caption().y > rows_bottom);
        let card = tray.card(0, 255);
        assert!(tray.caption().bottom() <= card.bottom());
        for (_, rect) in tray.rows() {
            assert!(rect.x >= card.x && rect.right() <= card.right());
        }
    }

    #[test]
    fn the_card_grows_only_by_what_the_face_cannot_hold() {
        let short = face_at(300, 100);
        let tall = Rect::new(300, 100, 200, 400);
        let grown = tray(short);
        let roomy = ActionTray::fit(&bar(tall), tall, FIELD, spacing(), CAPTION).expect("tray");
        assert!(grown.extra_at(255) > 0);
        assert_eq!(roomy.extra_at(255), 0, "a tall face already has the room");
        assert_eq!(grown.card(0, 255).h, short.h + grown.extra_at(255));
    }

    #[test]
    fn the_card_opens_by_degrees() {
        let tray = tray(face_at(300, 100));
        assert_eq!(tray.extra_at(0), 0);
        let half = tray.extra_at(128);
        assert!(half > 0 && half < tray.extra_at(255));
        assert_eq!(tray.card(0, 0), face_at(300, 100));
    }

    #[test]
    fn a_tile_near_the_bottom_slides_up_instead_of_overflowing() {
        let face = face_at(300, 600);
        let tray = tray(face);
        assert!(tray.slide_at(255) > 0);
        assert!(tray.card(0, 255).bottom() <= FIELD.bottom());
        assert!(tray.card(0, 255).y >= FIELD.y);
    }

    #[test]
    fn a_tile_with_room_below_does_not_slide() {
        assert_eq!(tray(face_at(300, 100)).slide_at(255), 0);
    }

    #[test]
    fn the_card_stays_inside_the_field_at_every_edge() {
        for (x, y) in [(0, 0), (800, 0), (0, 560), (800, 560), (400, 300)] {
            let card = tray(face_at(x, y)).card(0, 255);
            assert!(
                card.x >= FIELD.x && card.right() <= FIELD.right(),
                "{x},{y}"
            );
            assert!(
                card.y >= FIELD.y && card.bottom() <= FIELD.bottom(),
                "{x},{y}"
            );
        }
    }

    #[test]
    fn a_hit_names_the_row_button_where_it_is_drawn() {
        let tray = tray(face_at(300, 100));
        for lift in [0, 4] {
            for (action, rect) in tray.rows() {
                let centre = Point::new(rect.x + rect.w / 2, rect.y + rect.h / 2 - lift);
                assert_eq!(tray.hit(centre, lift, 255), Some(*action));
            }
        }
    }

    #[test]
    fn gaps_and_the_caption_hit_no_button() {
        let tray = tray(face_at(300, 100));
        let first = tray.rows()[0].1;
        assert_eq!(
            tray.hit(Point::new(first.right(), first.y + 1), 0, 255),
            None
        );
        let caption = tray.caption();
        assert_eq!(
            tray.hit(Point::new(caption.x + 1, caption.y + 1), 0, 255),
            None
        );
    }

    #[test]
    fn a_face_too_small_for_buttons_gets_no_tray() {
        let narrow = Rect::new(0, 0, 60, 140);
        assert!(ActionTray::fit(&bar(narrow), narrow, FIELD, spacing(), CAPTION).is_none());
    }

    #[test]
    fn a_larger_font_grows_the_rows() {
        let face = Rect::new(300, 100, 400, 300);
        let fit = |points| {
            let spacing = Spacing::new(points, 1.0);
            let bar = ActionBar::fit(face, face.y + TEXT_BOTTOM, spacing);
            ActionTray::fit(&bar, face, FIELD, spacing, CAPTION).expect("tray")
        };
        assert!(fit(26.0).rows()[0].1.w > fit(13.0).rows()[0].1.w);
    }
}
