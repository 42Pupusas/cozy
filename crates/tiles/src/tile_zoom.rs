//! [`Zoom`] -- the scaling that carries one rectangle onto another, applied
//! to the rectangles around it.

use platform::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Zoom {
    from: Rect,
    to: Rect,
}

impl Zoom {
    #[must_use]
    pub const fn new(from: Rect, to: Rect) -> Self {
        Self { from, to }
    }

    #[must_use]
    pub fn apply(&self, rect: Rect) -> Rect {
        let x = self.horizontal(rect.x);
        let y = self.vertical(rect.y);
        Rect::new(
            x,
            y,
            self.horizontal(rect.right()) - x,
            self.vertical(rect.bottom()) - y,
        )
    }

    fn horizontal(&self, x: i32) -> i32 {
        self.to.x + Self::scale(x - self.from.x, self.to.w, self.from.w)
    }

    fn vertical(&self, y: i32) -> i32 {
        self.to.y + Self::scale(y - self.from.y, self.to.h, self.from.h)
    }

    fn scale(offset: i32, to: i32, from: i32) -> i32 {
        if from <= 0 {
            return offset;
        }
        let scaled = (i64::from(offset) * i64::from(to)).div_euclid(i64::from(from));
        i32::try_from(scaled).unwrap_or(if scaled < 0 { i32::MIN } else { i32::MAX })
    }
}

#[cfg(test)]
mod tests {
    use super::Zoom;
    use platform::draw::Rect;

    const PORTAL: Rect = Rect::new(100, 100, 100, 50);
    const FIELD: Rect = Rect::new(0, 0, 1000, 500);

    #[test]
    fn the_source_lands_exactly_on_the_target() {
        assert_eq!(Zoom::new(PORTAL, FIELD).apply(PORTAL), FIELD);
        assert_eq!(Zoom::new(FIELD, PORTAL).apply(FIELD), PORTAL);
    }

    #[test]
    fn a_rect_inside_the_source_lands_inside_the_target() {
        let inside = Zoom::new(FIELD, PORTAL).apply(Rect::new(500, 250, 500, 250));
        assert_eq!(inside, Rect::new(150, 125, 50, 25));
    }

    #[test]
    fn a_neighbour_is_carried_off_past_the_target() {
        let right = Zoom::new(PORTAL, FIELD).apply(Rect::new(200, 100, 50, 50));
        assert!(right.x >= FIELD.right(), "{right:?}");
        let above = Zoom::new(PORTAL, FIELD).apply(Rect::new(100, 0, 100, 100));
        assert!(above.bottom() <= FIELD.y, "{above:?}");
    }

    #[test]
    fn an_empty_source_leaves_offsets_unscaled() {
        let flat = Rect::new(10, 10, 0, 0);
        let moved = Zoom::new(flat, Rect::new(20, 20, 0, 0)).apply(Rect::new(15, 15, 5, 5));
        assert_eq!(moved, Rect::new(25, 25, 5, 5));
    }
}
