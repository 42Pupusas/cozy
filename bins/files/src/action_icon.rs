//! [`ActionIcon`] -- the line art on a tile's action buttons.
//!
//! Drawn from lines and rounded outlines rather than font glyphs: no text
//! face is guaranteed to carry clipboard, scissors or pencil symbols, and
//! shapes scale with the button instead of with a font size.

use platform::draw::{Canvas, Color, Point, Rect};

use crate::tile_action::TileAction;

/// One icon, sized to the button it sits on.
pub struct ActionIcon {
    action: TileAction,
    bounds: Rect,
    ink: Color,
}

impl ActionIcon {
    /// The icon for `action`, drawn inside `button` in `ink`.
    #[must_use]
    pub const fn new(action: TileAction, button: Rect, ink: Color) -> Self {
        Self {
            action,
            bounds: button,
            ink,
        }
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>) {
        match self.action {
            TileAction::CopyPath => self.paint_link(canvas),
            TileAction::Copy => self.paint_pages(canvas),
            TileAction::Cut => self.paint_scissors(canvas),
            TileAction::Rename => self.paint_pencil(canvas),
        }
    }

    /// The square the art occupies: the button less a margin, forced odd so
    /// it has a centre pixel to hang symmetric strokes from.
    fn art(&self) -> Rect {
        let margin = (self.bounds.w / 4).max(2);
        let side = (self.bounds.w - margin * 2).max(3) | 1;
        Rect::new(
            self.bounds.x + (self.bounds.w - side) / 2,
            self.bounds.y + (self.bounds.h - side) / 2,
            side,
            side,
        )
    }

    fn stroke(&self) -> i32 {
        i32::from(self.bounds.w >= 30) + 1
    }

    fn line(&self, canvas: &mut Canvas<'_>, from: Point, to: Point) {
        canvas.draw_line(from.x, from.y, to.x, to.y, self.stroke(), self.ink);
    }

    /// A front page outline, with the top and right edges of a second page
    /// behind it.
    fn paint_pages(&self, canvas: &mut Canvas<'_>) {
        let art = self.art();
        let page = (art.w * 2 / 3).max(3);
        let radius = (art.w / 8).max(1);
        let front = Rect::new(art.x, art.y + art.h - page, page, page);
        canvas.draw_round_rect(front, self.ink, radius);
        let (left, right) = (art.x + 2, art.x + art.w - 1);
        self.line(canvas, Point::new(left, art.y), Point::new(right, art.y));
        self.line(
            canvas,
            Point::new(right, art.y),
            Point::new(right, art.y + art.h - page + page / 2),
        );
    }

    /// Two finger rings below, and two blades crossing above them.
    fn paint_scissors(&self, canvas: &mut Canvas<'_>) {
        let art = self.art();
        let ring = (art.w / 3).max(3);
        let left = Rect::new(art.x, art.y + art.h - ring, ring, ring);
        let right = Rect::new(art.x + art.w - ring, art.y + art.h - ring, ring, ring);
        canvas.draw_ellipse(left, self.ink);
        canvas.draw_ellipse(right, self.ink);
        let hinge = Point::new(art.x + art.w / 2, art.y + art.h / 2 + 1);
        self.line(
            canvas,
            Point::new(left.x + ring / 2, left.y + ring / 4),
            Point::new(art.x + art.w - 1, art.y),
        );
        self.line(
            canvas,
            Point::new(right.x + ring / 2, right.y + ring / 4),
            Point::new(art.x, art.y),
        );
        canvas.set_pixel(hinge.x, hinge.y, self.ink);
    }

    /// A pencil on the diagonal, its tip resting on a short baseline.
    fn paint_pencil(&self, canvas: &mut Canvas<'_>) {
        let art = self.art();
        let bottom = art.y + art.h - 1;
        let tip = Point::new(art.x + 1, bottom - 2);
        let end = Point::new(art.x + art.w - 1, art.y + 1);
        canvas.draw_line(tip.x, tip.y, end.x, end.y, self.stroke() + 1, self.ink);
        self.line(
            canvas,
            Point::new(art.x, bottom),
            Point::new(art.x + art.w / 2, bottom),
        );
    }

    /// Two chain links, side by side, with a join between them.
    fn paint_link(&self, canvas: &mut Canvas<'_>) {
        let art = self.art();
        let height = (art.h / 2).max(3);
        let width = (art.w * 5 / 9).max(3);
        let top = art.y + (art.h - height) / 2;
        let radius = height / 2;
        canvas.draw_round_rect(Rect::new(art.x, top, width, height), self.ink, radius);
        canvas.draw_round_rect(
            Rect::new(art.x + art.w - width, top, width, height),
            self.ink,
            radius,
        );
        let middle = top + height / 2;
        self.line(
            canvas,
            Point::new(art.x + art.w / 2 - 1, middle),
            Point::new(art.x + art.w / 2 + 1, middle),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::ActionIcon;
    use crate::tile_action::TileAction;
    use platform::draw::{Canvas, Color, Rect};

    const SIDE: i32 = 40;

    trait Inked {
        fn ink_count(&self) -> usize;
    }

    impl Inked for Vec<u32> {
        fn ink_count(&self) -> usize {
            self.iter().filter(|&&p| p != 0).count()
        }
    }

    fn drawn(action: TileAction, button: Rect) -> Vec<u32> {
        let mut buf = vec![0u32; (SIDE * SIDE) as usize];
        let mut canvas = Canvas::new(&mut buf, SIDE, SIDE);
        ActionIcon::new(action, button, Color::WHITE).paint(&mut canvas);
        buf
    }

    #[test]
    fn every_action_draws_something() {
        let button = Rect::new(6, 6, 28, 28);
        for action in TileAction::ALL {
            assert!(drawn(action, button).ink_count() > 8, "{action:?} is blank");
        }
    }

    #[test]
    fn the_icons_are_distinct_from_one_another() {
        let button = Rect::new(6, 6, 28, 28);
        let all: Vec<Vec<u32>> = TileAction::ALL.map(|a| drawn(a, button)).to_vec();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn an_icon_stays_inside_its_button() {
        let button = Rect::new(10, 10, 20, 20);
        for action in TileAction::ALL {
            let buf = drawn(action, button);
            for y in 0..SIDE {
                for x in 0..SIDE {
                    let inside = button.contains(platform::draw::Point::new(x, y));
                    if !inside {
                        assert_eq!(
                            buf[usize::try_from(y * SIDE + x).expect("in range")],
                            0,
                            "{action:?} inked outside at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_smallest_button_still_gets_an_icon() {
        let button = Rect::new(10, 10, 18, 18);
        for action in TileAction::ALL {
            assert!(drawn(action, button).ink_count() > 4, "{action:?} vanished");
        }
    }

    #[test]
    fn a_larger_button_draws_a_larger_icon() {
        for action in TileAction::ALL {
            let small = drawn(action, Rect::new(10, 10, 18, 18)).ink_count();
            let large = drawn(action, Rect::new(4, 4, 32, 32)).ink_count();
            assert!(large > small, "{action:?} did not grow");
        }
    }
}
