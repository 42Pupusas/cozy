//! Software drawing primitives over a raw pixel buffer.
//!
//! Everything visual the library does ultimately writes `u32`s into the
//! shared-memory buffer the compositor displays. This module is the layer that
//! turns "fill a rectangle in coral" into the right index math — no protocol, no
//! I/O, just pixels.
//!
//! Pixels are `XRGB8888`: `0x00RRGGBB`, one `u32` per pixel, the high byte
//! ignored by the compositor. Coordinates are **signed** and every primitive is
//! **clipped** to the buffer bounds, so drawing partly (or wholly) off-screen
//! paints only the visible part and never panics.

use crate::font;

/// An RGBA color. Alpha is carried for future use (blending, ARGB surfaces);
/// the current XRGB8888 buffer ignores it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Opaque color from 8-bit channels.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    /// Color with an explicit alpha channel.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Pack into the `0x00RRGGBB` word the XRGB8888 buffer expects.
    pub const fn to_xrgb8888(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Pack into the `0xAARRGGBB` word the ARGB8888 format expects (cursors,
    /// subsurfaces with per-pixel alpha).
    pub const fn to_argb8888(self) -> u32 {
        ((self.a as u32) << 24) | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Pack `(r, g, b)` at `alpha` into a **premultiplied** ARGB8888 word, as
    /// `wl_shm` requires for any format other than the fully-opaque case: the
    /// wire spec says "pre-multiplied alpha is used for pixel values" for
    /// every `wl_shm` format. `alpha == 255` degenerates to the identity
    /// (straight == premultiplied at full opacity), so callers can use this
    /// unconditionally without a fast-path split.
    #[must_use]
    pub fn premultiplied_argb8888(self, alpha: u8) -> u32 {
        let a = u32::from(alpha);
        let r = (u32::from(self.r) * a) / 255;
        let g = (u32::from(self.g) * a) / 255;
        let b = (u32::from(self.b) * a) / 255;
        (a << 24) | (r << 16) | (g << 8) | b
    }
}

/// A handful of named colors so call sites read in words, not hex.
impl Color {
    pub const BLACK: Color = Color::rgb(0x00, 0x00, 0x00);
    pub const WHITE: Color = Color::rgb(0xff, 0xff, 0xff);
    pub const SLATE: Color = Color::rgb(0x1e, 0x1e, 0x28); // the window background
    pub const CORAL: Color = Color::rgb(0xff, 0x5c, 0x57); // the box
    pub const TEAL: Color = Color::rgb(0x57, 0xc7, 0xff);
    /// Fully transparent black — the zero value in ARGB8888.
    pub const TRANSPARENT: Color = Color::rgba(0x00, 0x00, 0x00, 0x00);
}

/// A point in pixel space. Signed so it can sit off any edge.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle. Signed so it can extend off any edge; `w`/`h` are
/// widths (a zero or negative extent is an empty rect).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Exclusive right/bottom edges.
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Shrink the rect inward by `n` on every side (negative `n` grows it).
    pub const fn inset(&self, n: i32) -> Rect {
        Rect::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }

    /// Whether `p` lies inside the rect (left/top inclusive, right/bottom
    /// exclusive — matching how `fill_rect` paints).
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
}

/// A drawable view over a pixel buffer: the `u32` slice plus its dimensions.
///
/// Borrows the buffer mutably for the lifetime of the canvas, so drawing is just
/// safe slice writes. All methods clip to `[0,width) × [0,height)`.
pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
}

impl<'a> Canvas<'a> {
    /// Wrap a buffer. `pixels.len()` must be at least `width * height`.
    pub fn new(pixels: &'a mut [u32], width: i32, height: i32) -> Self {
        debug_assert!(width >= 0 && height >= 0);
        debug_assert!(pixels.len() >= (width as usize) * (height as usize));
        Self {
            pixels,
            width,
            height,
        }
    }

    pub fn width(&self) -> i32 {
        self.width
    }
    pub fn height(&self) -> i32 {
        self.height
    }

    /// The raw pixel buffer, mutable. Escape hatch for other modules (e.g.
    /// `render`'s grid-cell blitting) that need direct flat-index access
    /// rather than one of `Canvas`'s coordinate-based drawing primitives.
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// The whole canvas as a rect — handy for full-surface ops.
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Paint every pixel with `color`.
    pub fn clear(&mut self, color: Color) {
        let v = color.to_xrgb8888();
        for p in self.pixels.iter_mut() {
            *p = v;
        }
    }

    /// Set a single pixel, ignoring out-of-bounds coordinates.
    pub fn set_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let idx = (y as usize) * (self.width as usize) + (x as usize);
        self.pixels[idx] = color.to_xrgb8888();
    }

    /// Fill `rect` (clipped to the canvas) with `color`.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some((x0, y0, x1, y1)) = self.clip(rect) else {
            return;
        };
        let v = color.to_xrgb8888();
        let w = self.width as usize;
        for y in y0..y1 {
            let row = (y as usize) * w;
            for px in &mut self.pixels[row + x0 as usize..row + x1 as usize] {
                *px = v;
            }
        }
    }

    /// Draw the 1px outline of `rect` (clipped) in `color`.
    pub fn draw_rect(&mut self, rect: Rect, color: Color) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let top = rect.y;
        let bottom = rect.bottom() - 1;
        let left = rect.x;
        let right = rect.right() - 1;
        self.hline(left, right, top, color);
        self.hline(left, right, bottom, color);
        self.vline(top, bottom, left, color);
        self.vline(top, bottom, right, color);
    }

    /// Draw one glyph's `char` with its top-left at `(x, y)` in `color`, and
    /// return the x to start the next glyph at (`x + GLYPH_W`). Set bits in the
    /// bitmap become ink pixels; unset bits are left transparent (the background
    /// shows through). Characters outside the embedded table render as blank but
    /// still advance, so columns stay aligned. Clipped per-pixel via `set_pixel`.
    pub fn draw_char(&mut self, c: char, x: i32, y: i32, color: Color) -> i32 {
        // Only the printable-ASCII range is in the table; everything else (incl.
        // non-ASCII) draws blank but advances one cell.
        let code = c as u32;
        if (font::FIRST as u32..=font::LAST as u32).contains(&code) {
            let glyph = &font::FONT[(code as u8 - font::FIRST) as usize];
            for (row, bits) in glyph.iter().enumerate() {
                // MSB is the leftmost pixel.
                for col in 0..font::GLYPH_W {
                    if bits & (0x80 >> col) != 0 {
                        self.set_pixel(x + col, y + row as i32, color);
                    }
                }
            }
        }
        x + font::GLYPH_W
    }

    /// Draw `text` left-to-right starting at `(x, y)` (the top-left of the first
    /// glyph), in `color`. Advances one fixed cell per character; `\n` moves to
    /// the next line (back to the start `x`, down `GLYPH_H`). No wrapping —
    /// glyphs past the right edge are clipped away by `draw_char`.
    pub fn draw_text(&mut self, text: &str, x: i32, y: i32, color: Color) {
        let (mut cx, mut cy) = (x, y);
        for c in text.chars() {
            if c == '\n' {
                cx = x;
                cy += font::GLYPH_H;
            } else {
                cx = self.draw_char(c, cx, cy, color);
            }
        }
    }

    /// Horizontal line from `x0` to `x1` (inclusive) at row `y`, clipped.
    pub fn hline(&mut self, x0: i32, x1: i32, y: i32, color: Color) {
        if y < 0 || y >= self.height {
            return;
        }
        let (mut a, mut b) = (x0.min(x1), x0.max(x1));
        a = a.max(0);
        b = b.min(self.width - 1);
        if a > b {
            return;
        }
        let v = color.to_xrgb8888();
        let row = (y as usize) * (self.width as usize);
        for px in &mut self.pixels[row + a as usize..=row + b as usize] {
            *px = v;
        }
    }

    /// Vertical line from `y0` to `y1` (inclusive) at column `x`, clipped.
    pub fn vline(&mut self, y0: i32, y1: i32, x: i32, color: Color) {
        if x < 0 || x >= self.width {
            return;
        }
        let (mut a, mut b) = (y0.min(y1), y0.max(y1));
        a = a.max(0);
        b = b.min(self.height - 1);
        if a > b {
            return;
        }
        let v = color.to_xrgb8888();
        let w = self.width as usize;
        for y in a..=b {
            self.pixels[(y as usize) * w + x as usize] = v;
        }
    }

    /// Clip `rect` to the canvas, returning `(x0, y0, x1, y1)` exclusive bounds,
    /// or `None` if nothing is visible.
    fn clip(&self, rect: Rect) -> Option<(i32, i32, i32, i32)> {
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = rect.right().min(self.width);
        let y1 = rect.bottom().min(self.height);
        if x0 < x1 && y0 < y1 {
            Some((x0, y0, x1, y1))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas_buf(w: i32, h: i32) -> Vec<u32> {
        vec![0u32; (w * h) as usize]
    }

    #[test]
    fn rect_contains_is_left_top_inclusive_right_bottom_exclusive() {
        let r = Rect::new(10, 10, 20, 20); // covers [10,30) x [10,30)
        assert!(r.contains(Point::new(10, 10))); // top-left corner: inside
        assert!(r.contains(Point::new(29, 29))); // last inside pixel
        assert!(!r.contains(Point::new(30, 20))); // right edge: outside
        assert!(!r.contains(Point::new(20, 30))); // bottom edge: outside
        assert!(!r.contains(Point::new(9, 20))); // left of rect
        assert!(!r.contains(Point::new(20, 9))); // above rect
    }

    #[test]
    fn color_packs_to_xrgb() {
        assert_eq!(Color::rgb(0xff, 0x5c, 0x57).to_xrgb8888(), 0x00_ff_5c_57);
        assert_eq!(Color::BLACK.to_xrgb8888(), 0);
        assert_eq!(Color::WHITE.to_xrgb8888(), 0x00_ff_ff_ff);
    }

    #[test]
    fn fill_rect_writes_only_inside() {
        let mut buf = canvas_buf(4, 4);
        let mut c = Canvas::new(&mut buf, 4, 4);
        c.fill_rect(Rect::new(1, 1, 2, 2), Color::WHITE);
        let w = Color::WHITE.to_xrgb8888();
        // Interior 2x2 is white...
        assert_eq!(buf[4 + 1], w);
        assert_eq!(buf[8 + 2], w);
        // ...corners untouched.
        assert_eq!(buf[0], 0);
        assert_eq!(buf[3 * 4 + 3], 0);
    }

    #[test]
    fn fill_rect_clips_off_edges() {
        let mut buf = canvas_buf(4, 4);
        let mut c = Canvas::new(&mut buf, 4, 4);
        // Rect starting off the top-left, extending past it.
        c.fill_rect(Rect::new(-2, -2, 4, 4), Color::WHITE);
        let w = Color::WHITE.to_xrgb8888();
        // Only the visible 2x2 corner is painted.
        assert_eq!(buf[0], w);
        assert_eq!(buf[4 + 1], w);
        assert_eq!(buf[8 + 2], 0); // outside the clipped region
    }

    #[test]
    fn fully_offscreen_rect_is_noop() {
        let mut buf = canvas_buf(4, 4);
        let before = buf.clone();
        let mut c = Canvas::new(&mut buf, 4, 4);
        c.fill_rect(Rect::new(100, 100, 10, 10), Color::WHITE);
        c.fill_rect(Rect::new(-50, 0, 10, 4), Color::WHITE);
        assert_eq!(buf, before);
    }

    #[test]
    fn draw_rect_outline_only() {
        let mut buf = canvas_buf(5, 5);
        let mut c = Canvas::new(&mut buf, 5, 5);
        c.draw_rect(Rect::new(0, 0, 5, 5), Color::WHITE);
        let w = Color::WHITE.to_xrgb8888();
        // Border pixels set.
        assert_eq!(buf[0], w); // top-left
        assert_eq!(buf[4], w); // top-right
        assert_eq!(buf[4 * 5], w); // bottom-left
        assert_eq!(buf[4 * 5 + 4], w); // bottom-right
        // Center clear.
        assert_eq!(buf[2 * 5 + 2], 0);
    }

    #[test]
    fn set_pixel_ignores_out_of_bounds() {
        let mut buf = canvas_buf(2, 2);
        let mut c = Canvas::new(&mut buf, 2, 2);
        c.set_pixel(-1, 0, Color::WHITE);
        c.set_pixel(0, 5, Color::WHITE);
        c.set_pixel(1, 1, Color::WHITE);
        assert_eq!(buf[3], Color::WHITE.to_xrgb8888());
        assert_eq!(buf[0], 0);
    }

    #[test]
    fn rect_inset_shrinks() {
        assert_eq!(Rect::new(0, 0, 10, 10).inset(2), Rect::new(2, 2, 6, 6));
    }

    #[test]
    fn draw_char_blits_exactly_its_bitmap() {
        // Render 'A' at the origin and check every pixel against the font table:
        // an ink pixel where (and only where) a bit is set.
        let mut buf = canvas_buf(font::GLYPH_W, font::GLYPH_H);
        let mut c = Canvas::new(&mut buf, font::GLYPH_W, font::GLYPH_H);
        let advance = c.draw_char('A', 0, 0, Color::WHITE);
        assert_eq!(advance, font::GLYPH_W); // returns the next glyph's x

        let glyph = &font::FONT[(b'A' - font::FIRST) as usize];
        let ink = Color::WHITE.to_xrgb8888();
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..font::GLYPH_W {
                let set = bits & (0x80 >> col) != 0;
                let px = buf[row * font::GLYPH_W as usize + col as usize];
                assert_eq!(px == ink, set, "row {row} col {col}");
            }
        }
    }

    #[test]
    fn unknown_char_is_blank_but_advances() {
        // A char outside the table draws nothing yet still advances one cell, so
        // later glyphs stay column-aligned.
        let mut buf = canvas_buf(2 * font::GLYPH_W, font::GLYPH_H);
        let before = buf.clone();
        let mut c = Canvas::new(&mut buf, 2 * font::GLYPH_W, font::GLYPH_H);
        let advance = c.draw_char('€', 0, 0, Color::WHITE); // non-ASCII
        assert_eq!(advance, font::GLYPH_W);
        assert_eq!(buf, before); // nothing drawn
    }

    #[test]
    fn draw_text_clips_off_right_edge_without_panicking() {
        // A string wider than the canvas paints only the visible glyphs/parts.
        let mut buf = canvas_buf(font::GLYPH_W + 3, font::GLYPH_H);
        let mut c = Canvas::new(&mut buf, font::GLYPH_W + 3, font::GLYPH_H);
        c.draw_text("AB", 0, 0, Color::WHITE); // 'B' starts past the right edge
        // No panic is the main assertion; the first glyph still drew something.
        assert!(buf.iter().any(|&p| p == Color::WHITE.to_xrgb8888()));
    }
}
