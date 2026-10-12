//! [`CardGrip`] -- which part of the card a press grabs, the cursor that
//! shows it, and how dragging it reshapes the card.

use platform::draw::{Point, Rect};
use widget::CursorShape;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardGrip {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    Centre,
}

/// The smallest the card may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardFloor {
    pub w: i32,
    pub h: i32,
}

impl CardGrip {
    /// Which grip `at` takes on `card`, with the `band` pixels just inside
    /// an edge counting as that edge. Corners win over edges.
    #[must_use]
    pub const fn at(card: Rect, at: Point, band: i32) -> Option<Self> {
        if !card.contains(at) {
            return None;
        }
        let left = at.x < card.x + band;
        let right = at.x >= card.right() - band;
        let top = at.y < card.y + band;
        let bottom = at.y >= card.bottom() - band;
        Some(match (left, right, top, bottom) {
            (true, _, true, _) => Self::TopLeft,
            (_, true, true, _) => Self::TopRight,
            (true, _, _, true) => Self::BottomLeft,
            (_, true, _, true) => Self::BottomRight,
            (true, _, _, _) => Self::Left,
            (_, true, _, _) => Self::Right,
            (_, _, true, _) => Self::Top,
            (_, _, _, true) => Self::Bottom,
            _ => Self::Centre,
        })
    }

    #[must_use]
    pub const fn cursor(self) -> CursorShape {
        match self {
            Self::TopLeft | Self::BottomRight => CursorShape::ResizeNwse,
            Self::TopRight | Self::BottomLeft => CursorShape::ResizeNesw,
            Self::Left | Self::Right => CursorShape::ResizeEw,
            Self::Top | Self::Bottom => CursorShape::ResizeNs,
            Self::Centre => CursorShape::Move,
        }
    }

    const fn moves_left(self) -> bool {
        matches!(self, Self::TopLeft | Self::Left | Self::BottomLeft)
    }

    const fn moves_right(self) -> bool {
        matches!(self, Self::TopRight | Self::Right | Self::BottomRight)
    }

    const fn moves_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::Top | Self::TopRight)
    }

    const fn moves_bottom(self) -> bool {
        matches!(self, Self::BottomLeft | Self::Bottom | Self::BottomRight)
    }

    /// `start` dragged by `(dx, dy)` with this grip, kept inside `field` and
    /// no smaller than `floor`. The centre moves the whole card; an edge
    /// moves only itself, stopping where the card would get too small.
    #[must_use]
    pub fn drag(self, start: Rect, dx: i32, dy: i32, field: Rect, floor: CardFloor) -> Rect {
        if self == Self::Centre {
            let x = (start.x + dx).clamp(field.x, (field.right() - start.w).max(field.x));
            let y = (start.y + dy).clamp(field.y, (field.bottom() - start.h).max(field.y));
            return Rect::new(x, y, start.w, start.h);
        }
        let mut left = start.x;
        let mut right = start.right();
        let mut top = start.y;
        let mut bottom = start.bottom();
        if self.moves_left() {
            left = (left + dx).clamp(field.x, right - floor.w);
        }
        if self.moves_right() {
            right = (right + dx).clamp(left + floor.w, field.right());
        }
        if self.moves_top() {
            top = (top + dy).clamp(field.y, bottom - floor.h);
        }
        if self.moves_bottom() {
            bottom = (bottom + dy).clamp(top + floor.h, field.bottom());
        }
        Rect::new(left, top, right - left, bottom - top)
    }
}

#[cfg(test)]
mod tests {
    use super::{CardFloor, CardGrip};
    use platform::draw::{Point, Rect};
    use widget::CursorShape;

    const CARD: Rect = Rect::new(100, 100, 300, 200);
    const FIELD: Rect = Rect::new(0, 0, 1000, 800);
    const FLOOR: CardFloor = CardFloor { w: 200, h: 150 };

    #[test]
    fn corners_and_edges_are_grabbed_just_inside() {
        assert_eq!(CardGrip::at(CARD, Point::new(102, 104), 8), Some(CardGrip::TopLeft));
        assert_eq!(CardGrip::at(CARD, Point::new(398, 296), 8), Some(CardGrip::BottomRight));
        assert_eq!(CardGrip::at(CARD, Point::new(250, 103), 8), Some(CardGrip::Top));
        assert_eq!(CardGrip::at(CARD, Point::new(395, 200), 8), Some(CardGrip::Right));
    }

    #[test]
    fn the_middle_is_the_centre_and_far_away_is_nothing() {
        assert_eq!(CardGrip::at(CARD, Point::new(250, 200), 8), Some(CardGrip::Centre));
        assert_eq!(CardGrip::at(CARD, Point::new(600, 600), 8), None);
        assert_eq!(CardGrip::at(CARD, Point::new(96, 150), 8), None, "outside is a neighbour's");
    }

    #[test]
    fn each_grip_shows_its_cursor() {
        assert_eq!(CardGrip::Centre.cursor(), CursorShape::Move);
        assert_eq!(CardGrip::Left.cursor(), CursorShape::ResizeEw);
        assert_eq!(CardGrip::Bottom.cursor(), CursorShape::ResizeNs);
        assert_eq!(CardGrip::TopRight.cursor(), CursorShape::ResizeNesw);
        assert_eq!(CardGrip::TopLeft.cursor(), CursorShape::ResizeNwse);
    }

    #[test]
    fn the_centre_moves_the_card_without_resizing_it() {
        let moved = CardGrip::Centre.drag(CARD, 50, -30, FIELD, FLOOR);
        assert_eq!(moved, Rect::new(150, 70, 300, 200));
    }

    #[test]
    fn the_centre_stops_at_the_field_edge() {
        let moved = CardGrip::Centre.drag(CARD, 5000, -5000, FIELD, FLOOR);
        assert_eq!(moved, Rect::new(700, 0, 300, 200));
    }

    #[test]
    fn an_edge_moves_only_itself() {
        assert_eq!(
            CardGrip::Right.drag(CARD, 100, 999, FIELD, FLOOR),
            Rect::new(100, 100, 400, 200)
        );
        assert_eq!(
            CardGrip::Top.drag(CARD, 999, -50, FIELD, FLOOR),
            Rect::new(100, 50, 300, 250)
        );
    }

    #[test]
    fn a_corner_moves_both_its_edges() {
        assert_eq!(
            CardGrip::TopLeft.drag(CARD, -20, -40, FIELD, FLOOR),
            Rect::new(80, 60, 320, 240)
        );
    }

    #[test]
    fn a_resize_stops_at_the_floor_rather_than_flipping() {
        let shrunk = CardGrip::BottomRight.drag(CARD, -1000, -1000, FIELD, FLOOR);
        assert_eq!(shrunk, Rect::new(100, 100, FLOOR.w, FLOOR.h));
        let pushed = CardGrip::Left.drag(CARD, 1000, 0, FIELD, FLOOR);
        assert_eq!(pushed.right(), CARD.right());
        assert_eq!(pushed.w, FLOOR.w);
    }

    #[test]
    fn a_resize_stops_at_the_field_edge() {
        let grown = CardGrip::BottomRight.drag(CARD, 5000, 5000, FIELD, FLOOR);
        assert_eq!((grown.right(), grown.bottom()), (FIELD.right(), FIELD.bottom()));
    }
}
