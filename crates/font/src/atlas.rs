//! Glyph atlas: pack rasterized glyph bitmaps into a single 2-D texture.
//!
//! Uses the shelf-first-fit bin-packing algorithm — simple, fast, and
//! good enough for the (bounded) glyph set of a terminal.

use crate::raster::Bitmap;

/// UV region of a glyph within the atlas texture (pixel coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from pen position to the left edge of the bitmap.
    pub bearing_x: i32,
    /// Vertical offset from the cell's top (the face's rounded ascender row)
    /// down to the glyph bitmap's top edge.
    ///
    /// Deliberately *not* "distance from baseline to bitmap top" (which is
    /// what the rasterizer / `GlyphMetrics` calls `bearing_y`): that
    /// quantity and the face's ascender are each fractional, and rounding
    /// them to pixels *separately* before subtracting at blit time made
    /// every glyph jitter its baseline by up to a pixel independently of its
    /// neighbors (each glyph's fractional part rounds differently) — the
    /// "baseline looks a little off for most characters" bug. Callers must
    /// compute this as a single `(ascender - bearing_y).round()`, once, so
    /// the whole glyph moves as an atomic unit relative to the shared
    /// cell-top origin instead of two independently-rounded pieces fighting
    /// each other.
    pub bearing_y: i32,
    /// How far to advance the pen after this glyph.
    pub advance: i32,
}

/// A 2-D glyph texture atlas (single 8-bit alpha channel).
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    /// Row-major, 1 byte per pixel.
    pub data: Vec<u8>,

    // Shelf packing state.
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

impl Atlas {
    /// Create a new atlas with the given dimensions.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0u8; (width * height) as usize],
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
        }
    }

    /// Insert a glyph bitmap into the atlas.
    /// Returns the `GlyphEntry` describing its position, or `None` if the
    /// atlas is full.
    pub fn insert(
        &mut self,
        bmp: &Bitmap,
        bearing_x: i32,
        bearing_y: i32,
        advance: i32,
    ) -> Option<GlyphEntry> {
        let w = bmp.width;
        let h = bmp.height;

        if w == 0 || h == 0 {
            // Blank glyph (space, NUL…) — record metrics but no pixels.
            return Some(GlyphEntry {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                bearing_x,
                bearing_y,
                advance,
            });
        }

        // 1-pixel padding between glyphs.
        let padded_w = w + 1;
        let padded_h = h + 1;

        // Advance shelf if current row is full.
        if self.shelf_x + padded_w > self.width {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }

        if self.shelf_y + padded_h > self.height {
            return None; // atlas full
        }

        let gx = self.shelf_x;
        let gy = self.shelf_y;

        // Blit the bitmap into the atlas.
        for row in 0..h {
            let src_off = (row * w) as usize;
            let dst_off = ((gy + row) * self.width + gx) as usize;
            self.data[dst_off..dst_off + w as usize]
                .copy_from_slice(&bmp.data[src_off..src_off + w as usize]);
        }

        self.shelf_x += padded_w;
        self.shelf_h = self.shelf_h.max(padded_h);

        Some(GlyphEntry {
            x: gx,
            y: gy,
            width: w,
            height: h,
            bearing_x,
            bearing_y,
            advance,
        })
    }
}
