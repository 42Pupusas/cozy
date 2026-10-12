//! [`CardPlacement`] -- where the card sits and how big it is, kept the same
//! across folders and changed only by the user dragging it.

use platform::draw::{Point, Rect};

use crate::card_grip::{CardFloor, CardGrip};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Drag {
    grip: CardGrip,
    from: Point,
    start: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CardPlacement {
    chosen: Option<Rect>,
    drag: Option<Drag>,
}

impl CardPlacement {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            chosen: None,
            drag: None,
        }
    }

    /// The card's cell in `field`: where the user put it, or centred at its
    /// floor size until they do. Always inside `field`, shrunk to fit when
    /// the field is smaller than the card.
    #[must_use]
    pub fn cell(&self, field: Rect, floor: CardFloor) -> Rect {
        let wanted = self.chosen.map_or_else(
            || Self::centred(field, floor),
            |offset| Self::shifted(offset, field.x, field.y),
        );
        Self::fit(wanted, field)
    }

    /// A card the user already placed at `offset` from the field's corner.
    #[must_use]
    pub const fn at(offset: Rect) -> Self {
        Self {
            chosen: Some(offset),
            drag: None,
        }
    }

    /// Where the user put the card, measured from the field's corner.
    #[must_use]
    pub const fn offset(&self) -> Option<Rect> {
        self.chosen
    }

    const fn shifted(rect: Rect, dx: i32, dy: i32) -> Rect {
        Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h)
    }

    const fn centred(field: Rect, floor: CardFloor) -> Rect {
        Rect::new(
            field.x + (field.w - floor.w) / 2,
            field.y + (field.h - floor.h) / 2,
            floor.w,
            floor.h,
        )
    }

    fn fit(rect: Rect, field: Rect) -> Rect {
        let w = rect.w.clamp(0, field.w.max(0));
        let h = rect.h.clamp(0, field.h.max(0));
        let x = rect.x.clamp(field.x, field.right() - w);
        let y = rect.y.clamp(field.y, field.bottom() - h);
        Rect::new(x, y, w, h)
    }

    /// Start dragging `grip` from `at`, with the card at `cell`.
    pub const fn grab(&mut self, grip: CardGrip, at: Point, cell: Rect) {
        self.drag = Some(Drag {
            grip,
            from: at,
            start: cell,
        });
    }

    /// The grip being dragged, if any.
    #[must_use]
    pub fn held(&self) -> Option<CardGrip> {
        self.drag.map(|drag| drag.grip)
    }

    /// Carry the drag to `at`. Returns whether the card changed.
    pub fn pull(&mut self, at: Point, field: Rect, floor: CardFloor) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let moved = drag.grip.drag(
            drag.start,
            at.x - drag.from.x,
            at.y - drag.from.y,
            field,
            floor,
        );
        let offset = Self::shifted(moved, -field.x, -field.y);
        let changed = self.chosen != Some(offset);
        self.chosen = Some(offset);
        changed
    }

    /// End the drag. Returns the card's new offset from the field's corner
    /// when the drag moved or resized it.
    pub fn release(&mut self, field: Rect) -> Option<Rect> {
        let drag = self.drag.take()?;
        let start = Self::shifted(drag.start, -field.x, -field.y);
        self.chosen.filter(|&offset| offset != start)
    }
}

#[cfg(test)]
mod tests {
    use super::CardPlacement;
    use crate::card_grip::{CardFloor, CardGrip};
    use platform::draw::{Point, Rect};

    const FIELD: Rect = Rect::new(10, 10, 1000, 800);
    const FLOOR: CardFloor = CardFloor { w: 300, h: 200 };

    #[test]
    fn an_untouched_card_sits_in_the_middle_at_its_floor_size() {
        let cell = CardPlacement::new().cell(FIELD, FLOOR);
        assert_eq!(cell, Rect::new(360, 310, 300, 200));
    }

    #[test]
    fn dragging_the_centre_moves_the_card_and_it_stays_there() {
        let mut placement = CardPlacement::new();
        let start = placement.cell(FIELD, FLOOR);
        placement.grab(CardGrip::Centre, Point::new(400, 400), start);
        assert!(placement.pull(Point::new(300, 450), FIELD, FLOOR));
        let offset = placement.release(FIELD).expect("the card moved");
        assert_eq!(placement.held(), None);
        let moved = placement.cell(FIELD, FLOOR);
        assert_eq!(
            offset,
            Rect::new(moved.x - FIELD.x, moved.y - FIELD.y, moved.w, moved.h)
        );
        assert_eq!(placement.offset(), Some(offset));
        assert_eq!((moved.x, moved.y), (start.x - 100, start.y + 50));
        assert_eq!(placement.cell(FIELD, FLOOR), moved, "the place is kept");
    }

    #[test]
    fn dragging_an_edge_resizes_the_card() {
        let mut placement = CardPlacement::new();
        let start = placement.cell(FIELD, FLOOR);
        placement.grab(CardGrip::Right, Point::new(start.right(), 400), start);
        placement.pull(Point::new(start.right() + 80, 400), FIELD, FLOOR);
        assert_eq!(placement.cell(FIELD, FLOOR).w, start.w + 80);
    }

    #[test]
    fn pulling_without_a_grab_changes_nothing() {
        let mut placement = CardPlacement::new();
        assert!(!placement.pull(Point::new(0, 0), FIELD, FLOOR));
        assert_eq!(placement, CardPlacement::new());
    }

    #[test]
    fn a_pull_that_lands_where_the_card_already_is_reports_no_change() {
        let mut placement = CardPlacement::new();
        let start = placement.cell(FIELD, FLOOR);
        placement.grab(CardGrip::Centre, Point::new(400, 400), start);
        assert!(placement.pull(Point::new(410, 400), FIELD, FLOOR));
        assert!(!placement.pull(Point::new(410, 400), FIELD, FLOOR));
    }

    #[test]
    fn a_smaller_window_pulls_the_card_back_inside() {
        let mut placement = CardPlacement::new();
        let start = placement.cell(FIELD, FLOOR);
        placement.grab(CardGrip::Centre, Point::new(0, 0), start);
        placement.pull(Point::new(5000, 5000), FIELD, FLOOR);
        placement.release(FIELD);
        let small = Rect::new(10, 10, 600, 400);
        let cell = placement.cell(small, FLOOR);
        assert!(cell.right() <= small.right() && cell.bottom() <= small.bottom());
        assert!(cell.x >= small.x && cell.y >= small.y);
    }

    #[test]
    fn a_grab_released_where_it_started_reports_nothing_moved() {
        let mut placement = CardPlacement::new();
        let start = placement.cell(FIELD, FLOOR);
        placement.grab(CardGrip::Centre, Point::new(400, 400), start);
        placement.pull(Point::new(400, 400), FIELD, FLOOR);
        assert_eq!(placement.release(FIELD), None);
        assert_eq!(placement.release(FIELD), None, "no drag, nothing to report");
    }

    #[test]
    fn a_card_placed_earlier_starts_where_it_was_put_in_the_field() {
        let offset = Rect::new(20, 30, 400, 250);
        assert_eq!(
            CardPlacement::at(offset).cell(FIELD, FLOOR),
            Rect::new(30, 40, 400, 250)
        );
    }

    #[test]
    fn a_field_smaller_than_the_card_shrinks_it_to_fit() {
        let tiny = Rect::new(0, 0, 120, 90);
        assert_eq!(CardPlacement::new().cell(tiny, FLOOR), tiny);
    }
}
