//! Shared proportional-glyph blitting.
//!
//! Both the terminal renderer (`crate::render`, cell-snapped) and the status
//! bar (`statusbar::text`, pen-advanced) rasterize a TTF/OTF face into a
//! `font::Atlas` and then alpha-blend each cached glyph onto a software
//! `Canvas`. That *blend* step is byte-for-byte the same operation regardless
//! of how the caller positions glyphs -- so it lives here once instead of
//! being copy-pasted (the bar's copy was a plainer, unoptimized twin of the
//! terminal's hot-path version; unifying on the optimized one hands the bar
//! the same clip-once/checkless-inner-loop speedup for free).
//!
//! What stays with each caller is everything *around* the blend: the glyph
//! cache / fallback-face walk, the background fill (a cell rectangle for the
//! terminal, a full pre-cleared strip for the bar), and pen/cell advance.

use font::{Atlas, GlyphEntry};

use crate::draw::{Canvas, Color as DrawColor};

/// Extends [`Canvas`] with the shared glyph-blend kernel, as a method
/// instead of a free `blit_glyph(canvas, ...)` function -- `Canvas` is the
/// thing actually being mutated, so it's the natural owner.
impl Canvas<'_> {
    /// Alpha-blend one rasterized glyph from `atlas` onto this canvas.
    ///
    /// `origin_x`/`origin_y` are the glyph's pen origin *before* bearing --
    /// the cell's top-left for the terminal, the current pen position for
    /// the bar -- and [`GlyphEntry::bearing_x`]/`bearing_y` are added here
    /// (bearing_y is the already-resolved cell-top-to-glyph-top offset from
    /// `font::FontFace::cell_bearing_y`, *not* a baseline-relative value, so
    /// it is used directly and never recombined with a separately-rounded
    /// ascender).
    ///
    /// Glyph ink is always packed fully opaque regardless of `bg_alpha`
    /// ("translucent glass, solid text"): `premultiplied_argb8888(255)`
    /// degenerates to plain packed RGB when the buffer is opaque, so the
    /// `bg_alpha == 255` split is a no-op except on a translucent window.
    /// Anti-aliased edge pixels blend toward `bg` at their coverage `a`,
    /// honoring `bg_alpha` so edges stay correct over a translucent
    /// background.
    ///
    /// A blank glyph (zero width/height, e.g. a space) draws nothing -- the
    /// caller's background fill is the whole cell. Clipping to the canvas is
    /// done once per glyph (not per pixel): the valid column/row span is a
    /// per-glyph constant since the pen origin is row-independent, so the
    /// inner loop is a bounds-check-free write.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_glyph(
        &mut self,
        atlas: &Atlas,
        entry: &GlyphEntry,
        origin_x: i32,
        origin_y: i32,
        fg: DrawColor,
        bg: DrawColor,
        bg_alpha: u8,
    ) {
        if entry.width == 0 || entry.height == 0 {
            return; // blank glyph (space etc.) — the caller's bg fill suffices.
        }

        let buf_width = self.width() as u32;
        let fg_px = if bg_alpha == 255 {
            fg.to_xrgb8888()
        } else {
            fg.premultiplied_argb8888(255)
        };

        let glyph_x0 = i64::from(origin_x) + i64::from(entry.bearing_x);
        let glyph_y0 = i64::from(origin_y) + i64::from(entry.bearing_y);

        let pixels = self.pixels_mut();
        let atlas_w = atlas.width;
        let buf_h = (pixels.len() / buf_width.max(1) as usize) as i64;

        // Clip the glyph's column/row span to the buffer ONCE, not per
        // pixel: `glyph_x0`/`glyph_y0` are row/column-independent, so
        // `[col_lo, col_hi)` and `[row_lo, row_hi)` are per-glyph constants
        // and the inner loop needs no bounds check.
        let col_lo = (-glyph_x0).max(0) as u32;
        let col_hi = (i64::from(buf_width) - glyph_x0).clamp(0, i64::from(entry.width)) as u32;
        let row_lo = (-glyph_y0).max(0) as u32;
        let row_hi = (buf_h - glyph_y0).clamp(0, i64::from(entry.height)) as u32;
        if col_lo >= col_hi || row_lo >= row_hi {
            return;
        }

        for row in row_lo..row_hi {
            let py = (glyph_y0 + i64::from(row)) as usize;
            // Hoist the per-row bases out of the column loop: both the
            // destination row base (`py * buf_width`) and the atlas source
            // row base (`(entry.y + row) * atlas_w`) are per-row constants.
            let dst_base = py * buf_width as usize + (glyph_x0 + i64::from(col_lo)) as usize;
            let atlas_base = ((entry.y + row) * atlas_w) as usize + (entry.x + col_lo) as usize;
            for i in 0..(col_hi - col_lo) as usize {
                let a = atlas.data[atlas_base + i];
                if a == 0 {
                    continue;
                }
                let dst = &mut pixels[dst_base + i];
                *dst = if a == 255 {
                    fg_px
                } else {
                    fg.blend_glyph(bg, a, bg_alpha)
                };
            }
        }
    }
}
