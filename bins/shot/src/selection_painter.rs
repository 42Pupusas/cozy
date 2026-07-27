//! Paints the selection overlay: a dimmed screen with the chosen region cut
//! back out of it, plus a border and crosshair.
//!
//! Separate from [`crate::drag_selection::DragSelection`] (which decides *what*
//! is selected) and from the surface (which owns Wayland objects): this module
//! only turns a rectangle into pixels, so it can be tested by drawing into a
//! plain buffer and reading the result back.

use crate::drag_selection::DragSelection;
use platform::draw::{Canvas, Color, Rect};

/// Draws the overlay's contents onto a canvas.
///
/// Colours are chosen so the overlay reads over both a dark and a light
/// desktop: a neutral dim, a bright border, and a crosshair that stays visible
/// against the undimmed selection.
pub struct SelectionPainter {
    /// How dark the unselected area gets, 0-255 alpha over the desktop.
    dim_alpha: u8,
    border: Color,
    crosshair: Color,
}

impl Default for SelectionPainter {
    fn default() -> Self {
        Self {
            // Enough to clearly separate chosen from unchosen without hiding
            // what's underneath -- the user is aiming at something they need
            // to still be able to see.
            dim_alpha: 0x99,
            border: Color::rgb(0x57, 0xc7, 0xff),
            crosshair: Color::rgb(0xff, 0xff, 0xff),
        }
    }
}

impl SelectionPainter {
    /// Border thickness in pixels; thick enough to see, thin enough not to
    /// obscure the edge of what's being selected.
    const BORDER: i32 = 2;

    /// Paint the whole overlay for the current state of `selection`.
    ///
    /// The surface is ARGB, so "dim" is a real translucent fill rather than a
    /// blend against a known background -- the desktop showing through *is*
    /// the compositor's own compositing.
    pub fn paint(&self, canvas: &mut Canvas, selection: &DragSelection) {
        let dim = Color::rgba(0x00, 0x00, 0x00, self.dim_alpha);
        match selection.rect() {
            // Nothing selected yet, or a drag with no area: dim everything and
            // show where the pointer is, so the overlay is visibly live before
            // the first press. `clear` is deliberately not used -- it packs
            // XRGB and would zero the alpha, making the surface invisible.
            None => {
                canvas.fill_rect_argb(canvas.bounds(), dim);
                self.paint_crosshair(canvas, selection);
            }
            Some(rect) if rect.w <= 0 || rect.h <= 0 => {
                canvas.fill_rect_argb(canvas.bounds(), dim);
                self.paint_crosshair(canvas, selection);
            }
            Some(rect) => {
                // Dim only *around* the selection. Clearing everything and
                // then re-clearing the middle would not work: these are
                // translucent pixels written into an ARGB buffer, not blends,
                // so the selection must simply never be painted over.
                Self::paint_dim_around(canvas, rect, dim);
                canvas.fill_rect_argb(rect, Color::TRANSPARENT);
                self.paint_border(canvas, rect);
            }
        }
    }

    /// Fill the four bands surrounding `rect`, leaving `rect` untouched.
    fn paint_dim_around(canvas: &mut Canvas, rect: Rect, dim: Color) {
        let full = canvas.bounds();
        // Top and bottom span the full width; left and right cover only the
        // remaining slice, so no pixel is written twice (which would double
        // the alpha and show as seams).
        canvas.fill_rect_argb(Rect::new(0, 0, full.w, rect.y), dim);
        canvas.fill_rect_argb(
            Rect::new(0, rect.bottom(), full.w, full.h - rect.bottom()),
            dim,
        );
        canvas.fill_rect_argb(Rect::new(0, rect.y, rect.x, rect.h), dim);
        canvas.fill_rect_argb(
            Rect::new(rect.right(), rect.y, full.w - rect.right(), rect.h),
            dim,
        );
    }

    /// Outline the selection, drawn *inside* the rectangle so the border
    /// never covers a pixel that lies outside what will be captured.
    fn paint_border(&self, canvas: &mut Canvas, rect: Rect) {
        let t = Self::BORDER.min(rect.w).min(rect.h);
        canvas.fill_rect_argb(Rect::new(rect.x, rect.y, rect.w, t), self.border);
        canvas.fill_rect_argb(Rect::new(rect.x, rect.bottom() - t, rect.w, t), self.border);
        canvas.fill_rect_argb(Rect::new(rect.x, rect.y, t, rect.h), self.border);
        canvas.fill_rect_argb(Rect::new(rect.right() - t, rect.y, t, rect.h), self.border);
    }

    /// Full-width/height hairlines through the pointer, so the user can line
    /// up an edge before pressing.
    fn paint_crosshair(&self, canvas: &mut Canvas, selection: &DragSelection) {
        let at = selection.cursor();
        let full = canvas.bounds();
        canvas.fill_rect_argb(Rect::new(0, at.y, full.w, 1), self.crosshair);
        canvas.fill_rect_argb(Rect::new(at.x, 0, 1, full.h), self.crosshair);
    }
}

#[cfg(test)]
mod tests {
    use super::SelectionPainter;
    use crate::drag_selection::DragSelection;
    use platform::draw::{Canvas, Point};

    /// A test-owned ARGB buffer.
    ///
    /// Exists so the tests can address pixels by signed `i32` coordinates --
    /// the same type the painter works in -- while keeping the one
    /// sign-losing conversion in a single checked place instead of scattering
    /// `as usize` casts through every test.
    struct Surface {
        pixels: Vec<u32>,
        width: i32,
        height: i32,
    }

    impl Surface {
        fn new(width: i32, height: i32) -> Self {
            let count = usize::try_from(width * height).expect("test surface has positive area");
            Self {
                pixels: vec![0u32; count],
                width,
                height,
            }
        }

        /// Paint `selection` onto this surface with the default painter.
        fn paint(&mut self, selection: &DragSelection) {
            let mut canvas = Canvas::new(&mut self.pixels, self.width, self.height);
            SelectionPainter::default().paint(&mut canvas, selection);
        }

        /// Read one pixel's alpha byte out of the ARGB buffer.
        fn alpha_at(&self, x: i32, y: i32) -> u8 {
            let index = usize::try_from(y * self.width + x).expect("coordinates are in bounds");
            u8::try_from((self.pixels[index] >> 24) & 0xff).expect("one byte")
        }
    }

    /// The point of the whole overlay: what you selected is not dimmed, and
    /// what you didn't select is. A regression here makes the tool unusable
    /// even though it would still capture correctly.
    #[test]
    fn the_selection_is_clear_and_everything_else_is_dimmed() {
        let mut surface = Surface::new(200, 100);
        let mut selection = DragSelection::new(200, 100);
        selection.press(Point::new(50, 25));
        selection.motion(Point::new(150, 75));
        surface.paint(&selection);

        // Inside the band, away from the border, must be fully transparent.
        assert_eq!(surface.alpha_at(100, 50), 0x00);
        // Outside must carry the dim alpha.
        assert_eq!(surface.alpha_at(10, 10), 0x99);
        assert_eq!(surface.alpha_at(190, 90), 0x99);
    }

    /// The dim bands must tile the area around the selection exactly once --
    /// overlapping them would double-darken the seams into visible bars.
    #[test]
    fn the_dim_is_uniform_with_no_double_painted_seams() {
        let (w, h) = (120, 80);
        let mut surface = Surface::new(w, h);
        let mut selection = DragSelection::new(w, h);
        selection.press(Point::new(40, 30));
        selection.motion(Point::new(80, 50));
        surface.paint(&selection);

        // Every pixel outside the selection should have exactly one dim
        // alpha, including the corners where the four bands meet.
        for y in 0..h {
            for x in 0..w {
                let inside = (40..80).contains(&x) && (30..50).contains(&y);
                if !inside {
                    assert_eq!(
                        surface.alpha_at(x, y),
                        0x99,
                        "pixel ({x},{y}) outside the selection is not evenly dimmed"
                    );
                }
            }
        }
    }

    /// Before the first press there is no band, but the overlay must still
    /// be visibly present rather than a blank invisible surface.
    #[test]
    fn before_any_press_the_screen_is_dimmed_with_a_crosshair() {
        let mut surface = Surface::new(100, 60);
        let mut selection = DragSelection::new(100, 60);
        selection.motion(Point::new(30, 20));
        surface.paint(&selection);

        assert_eq!(surface.alpha_at(80, 50), 0x99, "screen should be dim");
        // The crosshair lines are opaque white where they cross the dim.
        assert_eq!(surface.alpha_at(70, 20), 0xff, "horizontal hairline");
        assert_eq!(surface.alpha_at(30, 50), 0xff, "vertical hairline");
    }

    /// The border is drawn inside the selection, so it can never darken a
    /// pixel that lies outside the region about to be captured.
    #[test]
    fn the_border_stays_inside_the_selection() {
        let mut surface = Surface::new(100, 100);
        let mut selection = DragSelection::new(100, 100);
        selection.press(Point::new(20, 20));
        selection.motion(Point::new(60, 60));
        surface.paint(&selection);

        // Just inside the edge is border (opaque), just outside is dim.
        assert_eq!(surface.alpha_at(20, 40), 0xff, "border on the left edge");
        assert_eq!(surface.alpha_at(19, 40), 0x99, "outside stays dim");
    }

    /// A degenerate drag must not panic or paint a broken band.
    #[test]
    fn a_zero_area_drag_paints_the_plain_dimmed_screen() {
        let mut surface = Surface::new(60, 40);
        let mut selection = DragSelection::new(60, 40);
        selection.press(Point::new(30, 20));
        selection.motion(Point::new(30, 20));
        surface.paint(&selection);

        assert_eq!(surface.alpha_at(5, 5), 0x99);
    }
}
