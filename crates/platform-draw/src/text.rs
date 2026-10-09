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

/// The shared glyph-blend kernel for [`Canvas`].
///
/// A trait, not an inherent `impl Canvas`, so that `Canvas` and this blend
/// kernel can live in different crates: Rust's orphan rule forbids an
/// inherent impl -- or a foreign trait's impl -- on a type from outside the
/// crate that defines it, but it always permits implementing a *local*
/// trait for a foreign type. `text.rs` and `draw.rs` moved into `platform-draw`
/// together (the third cut along the `sys`/`wire`/`draw` seam -- see this
/// crate's `lib.rs`), so today nothing forces the split this trait
/// anticipates; it stays a trait anyway; a future reshuffle that pulls
/// glyph blitting toward `font` (it only touches `font::Atlas`/`GlyphEntry`
/// and a `Canvas`, nothing else in this crate) or `Canvas` toward its own
/// crate again costs nothing extra to support.
pub trait GlyphBlit {
    /// Alpha-blend one rasterized glyph from `atlas` onto this canvas.
    ///
    /// `origin_x`/`origin_y` are the glyph's pen origin *before* bearing --
    /// the cell's top-left for the terminal, the current pen position for
    /// the bar -- and [`GlyphEntry::bearing_x`]/`bearing_y` are added here
    /// (`bearing_y` is the already-resolved cell-top-to-glyph-top offset from
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
    fn blit_glyph(
        &mut self,
        atlas: &Atlas,
        entry: &GlyphEntry,
        origin_x: i32,
        origin_y: i32,
        fg: DrawColor,
        bg: DrawColor,
        bg_alpha: u8,
    );
}

impl GlyphBlit for Canvas<'_> {
    #[allow(clippy::too_many_arguments)]
    fn blit_glyph(
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

        let buf_width = self.width().cast_unsigned();
        let solid_background = bg_alpha == 255 && !self.format().needs_alpha();
        let fg_px = if solid_background {
            fg.to_xrgb8888()
        } else {
            fg.premultiplied_argb8888(255)
        };
        let opaque_edge = if solid_background { 0 } else { 0xff00_0000 };

        let pen_x0 = i64::from(origin_x) + i64::from(entry.bearing_x);
        let top_y = i64::from(origin_y) + i64::from(entry.bearing_y);

        let pixels = self.pixels_mut();
        let atlas_w = atlas.width;
        let buf_h = i64::try_from(pixels.len() / usize::try_from(buf_width.max(1)).unwrap_or(1))
            .unwrap_or(0);

        // Clip the glyph's column/row span to the buffer ONCE, not per
        // pixel: `pen_x0`/`pen_y0` are row/column-independent, so
        // `[col_lo, col_hi)` and `[row_lo, row_hi)` are per-glyph constants
        // and the inner loop needs no bounds check. Each is a `.clamp(0, N)`
        // result, always `>= 0` and `<= N`, so the `u32` conversions below
        // are infallible.
        let col_lo = u32::try_from((-pen_x0).max(0)).unwrap_or(0);
        let col_hi =
            u32::try_from((i64::from(buf_width) - pen_x0).clamp(0, i64::from(entry.width)))
                .unwrap_or(0);
        let row_lo = u32::try_from((-top_y).max(0)).unwrap_or(0);
        let row_hi = u32::try_from((buf_h - top_y).clamp(0, i64::from(entry.height))).unwrap_or(0);
        if col_lo >= col_hi || row_lo >= row_hi {
            return;
        }

        for row in row_lo..row_hi {
            // Always `>= 0`: `row_lo..row_hi` is bounded by the clamp above.
            let py = usize::try_from(top_y + i64::from(row)).unwrap_or(0);
            // Hoist the per-row bases out of the column loop: both the
            // destination row base (`py * buf_width`) and the atlas source
            // row base (`(entry.y + row) * atlas_w`) are per-row constants.
            let dst_base =
                py * buf_width as usize + usize::try_from(pen_x0 + i64::from(col_lo)).unwrap_or(0);
            let atlas_base = ((entry.y + row) * atlas_w) as usize + (entry.x + col_lo) as usize;
            for i in 0..(col_hi - col_lo) as usize {
                let a = atlas.data[atlas_base + i];
                if a == 0 {
                    continue;
                }
                let dst = &mut pixels[dst_base + i];
                *dst = if a == 255 {
                    fg_px
                } else if bg_alpha == 255 {
                    fg.blend_glyph(bg, a, bg_alpha) | opaque_edge
                } else {
                    fg.blend_glyph(bg, a, bg_alpha)
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use font::{Atlas, GlyphEntry};

    use super::GlyphBlit;
    use crate::SurfaceFormat;
    use crate::draw::{Canvas, Color};

    struct Glyph;

    impl Glyph {
        const COVERAGE: u8 = 100;

        fn atlas() -> Atlas {
            let mut atlas = Atlas::new(1, 1);
            atlas.data[0] = Self::COVERAGE;
            atlas
        }

        fn entry() -> GlyphEntry {
            GlyphEntry {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                bearing_x: 0,
                bearing_y: 0,
                advance: 1,
            }
        }

        fn edge_pixel(format: SurfaceFormat) -> u32 {
            let mut pixels = [0u32; 1];
            let mut canvas = Canvas::new(&mut pixels, 1, 1).with_format(format);
            canvas.blit_glyph(
                &Self::atlas(),
                &Self::entry(),
                0,
                0,
                Color::WHITE,
                Color::BLACK,
                255,
            );
            pixels[0]
        }
    }

    #[test]
    fn an_edge_pixel_on_an_opaque_surface_carries_no_alpha_byte() {
        assert_eq!(Glyph::edge_pixel(SurfaceFormat::opaque()) >> 24, 0);
    }

    #[test]
    fn an_edge_pixel_over_a_solid_background_on_an_alpha_surface_is_opaque() {
        let pixel = Glyph::edge_pixel(SurfaceFormat::with_bg_alpha(200));
        assert_eq!(pixel >> 24, 0xff);
    }

    #[test]
    fn the_edge_colour_is_the_same_on_both_surfaces() {
        let opaque = Glyph::edge_pixel(SurfaceFormat::opaque());
        let glass = Glyph::edge_pixel(SurfaceFormat::with_bg_alpha(200));
        assert_eq!(opaque & 0x00ff_ffff, glass & 0x00ff_ffff);
    }
}
