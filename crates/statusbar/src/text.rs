//! Text rendering for the bar: either the embedded 8×16 bitmap font (no
//! config, always available) or a real TTF/OTF face loaded via `font`,
//! rasterized into a growing glyph atlas exactly like
//! `platform::render`'s `GlyphCache` -- just decoupled from the terminal's
//! fixed-width cell grid, since bar text is proportional-ish (still
//! monospace-metric in practice, but drawn glyph-by-glyph at each glyph's
//! own advance rather than snapped to a `cell_w` grid column).

use std::collections::HashMap;

use font::{Atlas, FontFace, GlyphEntry};
use platform::draw::{Canvas, Color};
use platform::font as bitmap_font;

/// Chosen once at bar startup from `[bar] font_path`/`font_size` (mirroring
/// `[font]` when unset) -- see `config::Config::bar_font_path`.
pub enum TextRenderer {
    /// The embedded bitmap font: fixed `GLYPH_W`×`GLYPH_H` cells, ASCII-only.
    Bitmap,
    /// A loaded TTF/OTF face with its own growing glyph atlas.
    Face(Box<FaceRenderer>),
}

pub struct FaceRenderer {
    face: FontFace,
    atlas: Atlas,
    map: HashMap<u32, GlyphEntry>,
    ascender: f32,
    /// Line height in pixels -- used to vertically center text in the bar.
    cell_height: f32,
}

impl TextRenderer {
    /// Load `path` at `px_size`; falls back to the embedded bitmap font
    /// (with a diagnostic on stderr) if `path` is `None` or fails to load.
    #[must_use]
    pub fn load(path: Option<&str>, px_size: f32) -> Self {
        let Some(path) = path else {
            return Self::Bitmap;
        };
        match FontFace::load(path, px_size) {
            Ok(mut face) => {
                let ascender = face.line.ascender;
                let cell_height = face.line.cell_height;
                let (atlas, map) = face.build_ascii_atlas(2048);
                Self::Face(Box::new(FaceRenderer {
                    face,
                    atlas,
                    map,
                    ascender,
                    cell_height,
                }))
            }
            Err(e) => {
                eprintln!(
                    "statusbar: failed to load font {path:?}: {e} — falling back to embedded bitmap font"
                );
                Self::Bitmap
            }
        }
    }

    /// Line height in pixels, for vertically centering a row of text in a
    /// bar of a given thickness.
    #[must_use]
    pub fn line_height(&self) -> i32 {
        match self {
            Self::Bitmap => bitmap_font::GLYPH_H,
            Self::Face(f) => f.cell_height.round().max(1.0) as i32,
        }
    }

    /// Y to pass as `draw_text`'s `y` so one line of text is vertically
    /// centered within a bar `bar_height` pixels thick.
    #[must_use]
    pub fn baseline_y(&self, bar_height: i32) -> i32 {
        ((bar_height - self.line_height()) / 2).max(0)
    }

    /// Width in pixels `text` would occupy if drawn now (rasterizing any
    /// not-yet-cached glyphs as a side effect, same as `draw_text`).
    pub fn measure(&mut self, text: &str) -> i32 {
        match self {
            Self::Bitmap => text.chars().count() as i32 * bitmap_font::GLYPH_W,
            Self::Face(f) => {
                let mut w = 0.0f32;
                for ch in text.chars() {
                    w += f.entry(ch as u32).map_or(0.0, |e| e.advance as f32);
                }
                w.round() as i32
            }
        }
    }

    /// Draw `text` left-to-right with its top-left (bitmap font) / baseline
    /// row (face font) at `(x, y)`, over an already-painted `bg` (needed for
    /// anti-aliased edge blending on the face path -- the bitmap path
    /// ignores it, being 1-bit). Returns the x just past the last glyph.
    pub fn draw_text(&mut self, canvas: &mut Canvas, text: &str, x: i32, y: i32, fg: Color, bg: Color) -> i32 {
        match self {
            Self::Bitmap => {
                canvas.draw_text(text, x, y, fg);
                x + text.chars().count() as i32 * bitmap_font::GLYPH_W
            }
            Self::Face(f) => {
                let mut pen_x = x;
                for ch in text.chars() {
                    let Some(entry) = f.entry(ch as u32) else {
                        continue;
                    };
                    if entry.width > 0 && entry.height > 0 {
                        blit_glyph(canvas, &f.atlas, &entry, pen_x, y, fg, bg);
                    }
                    pen_x += entry.advance;
                }
                pen_x
            }
        }
    }

    /// Right-align `text` so its last glyph ends at `right_x`.
    pub fn draw_text_right(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right_x: i32,
        y: i32,
        fg: Color,
        bg: Color,
    ) {
        let w = self.measure(text);
        self.draw_text(canvas, text, right_x - w, y, fg, bg);
    }
}

impl FaceRenderer {
    /// Look up (rasterizing + atlas-inserting on first use) the glyph entry
    /// for `codepoint`. `None` if the atlas is full.
    fn entry(&mut self, codepoint: u32) -> Option<GlyphEntry> {
        if let Some(e) = self.map.get(&codepoint) {
            return Some(*e);
        }
        let g = self.face.rasterize(codepoint);
        let entry = self.atlas.insert(
            &g.bitmap,
            g.metrics.bearing_x.round() as i32,
            FontFace::cell_bearing_y(self.ascender, g.metrics.bearing_y),
            g.metrics.advance.round() as i32,
        );
        if let Some(e) = entry {
            self.map.insert(codepoint, e);
        }
        entry
    }
}

/// Alpha-blend one rasterized glyph from `atlas` onto `canvas` at pen
/// position `(pen_x, pen_y)` (`pen_y` = the line's top row, matching
/// `entry.bearing_y`'s convention -- see `font::atlas::GlyphEntry`).
fn blit_glyph(
    canvas: &mut Canvas,
    atlas: &Atlas,
    entry: &GlyphEntry,
    pen_x: i32,
    pen_y: i32,
    fg: Color,
    bg: Color,
) {
    let buf_width = canvas.width();
    let buf_height = canvas.height();
    let glyph_x0 = pen_x + entry.bearing_x;
    let glyph_top = pen_y + entry.bearing_y;

    let col_lo = (-glyph_x0).max(0);
    let col_hi = (buf_width - glyph_x0).clamp(0, entry.width as i32);
    let row_lo = (-glyph_top).max(0);
    let row_hi = (buf_height - glyph_top).clamp(0, entry.height as i32);
    if col_lo >= col_hi || row_lo >= row_hi {
        return;
    }

    let pixels = canvas.pixels_mut();
    let atlas_w = atlas.width as i32;
    let fg_px = fg.to_xrgb8888();

    for row in row_lo..row_hi {
        let py = (glyph_top + row) as usize;
        let dst_row_base = py * buf_width as usize;
        let atlas_row_base = ((entry.y as i32 + row) * atlas_w) as usize;
        for col in col_lo..col_hi {
            let a = atlas.data[atlas_row_base + (entry.x as i32 + col) as usize];
            if a == 0 {
                continue;
            }
            let dst_idx = dst_row_base + (glyph_x0 + col) as usize;
            let Some(dst) = pixels.get_mut(dst_idx) else {
                continue;
            };
            *dst = if a == 255 {
                fg_px
            } else {
                fg.blend_glyph(bg, a, 255)
            };
        }
    }
}
