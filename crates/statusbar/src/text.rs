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
use platform::text::blit_glyph;

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
    /// Fallback faces (e.g. Noto Emoji, a Nerd Font for icon glyphs)
    /// consulted in order for codepoints `face` doesn't cover -- mirrors
    /// `platform::render`'s `GlyphCache`. Empty when no fallback fonts are
    /// configured (or all failed to load), in which case an uncovered
    /// codepoint rasterizes `face`'s own `.notdef`.
    fallbacks: Vec<FontFace>,
}

impl TextRenderer {
    /// Load `path` at `px_size`; falls back to the embedded bitmap font
    /// (with a diagnostic on stderr) if `path` is `None` or fails to load.
    ///
    /// Equivalent to `Self::load_with_fallbacks(path, &[], px_size)`.
    #[must_use]
    pub fn load(path: Option<&str>, px_size: f32) -> Self {
        Self::load_with_fallbacks(path, &[], px_size)
    }

    /// Load `path` at `px_size`, with an optional `emoji_path` fallback face
    /// consulted for codepoints `path`'s face doesn't cover (mirrors
    /// `config::Config::bar_emoji_path`/`platform::Renderer`'s emoji
    /// fallback). Falls back to the embedded bitmap font (with a
    /// diagnostic on stderr) if `path` is `None` or fails to load; a bad or
    /// missing `emoji_path` just means no fallback (also diagnosed), not a
    /// hard failure.
    ///
    /// Equivalent to `Self::load_with_fallbacks(path, &[emoji_path], px_size)`.
    #[must_use]
    pub fn load_with_emoji(path: Option<&str>, emoji_path: Option<&str>, px_size: f32) -> Self {
        Self::load_with_fallbacks(path, &[emoji_path], px_size)
    }

    /// Load `path` at `px_size`, consulting `fallback_paths` in order for
    /// codepoints `path`'s face doesn't cover -- e.g. an emoji font and a
    /// Nerd Font for icon glyphs, mirroring waybar's PUA icon set (see
    /// `modules.rs`). `None` entries in `fallback_paths` are skipped. Falls
    /// back to the embedded bitmap font (with a diagnostic on stderr) if
    /// `path` is `None` or fails to load; a bad or missing fallback path
    /// just means one less fallback (also diagnosed), not a hard failure.
    #[must_use]
    pub fn load_with_fallbacks(
        path: Option<&str>,
        fallback_paths: &[Option<&str>],
        px_size: f32,
    ) -> Self {
        let Some(path) = path else {
            return Self::Bitmap;
        };
        match FontFace::load(path, px_size) {
            Ok(mut face) => {
                let ascender = face.line.ascender;
                let cell_height = face.line.cell_height;
                let (atlas, map) = face.build_ascii_atlas(2048);
                let fallbacks = fallback_paths
                    .iter()
                    .filter_map(|p| Self::load_fallback(*p, px_size))
                    .collect();
                Self::Face(Box::new(FaceRenderer {
                    face,
                    atlas,
                    map,
                    ascender,
                    cell_height,
                    fallbacks,
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

    /// Load one fallback face, if `path` is set. Diagnoses (but doesn't
    /// abort on) a bad path, same treatment as the primary face's own
    /// failure handling.
    fn load_fallback(path: Option<&str>, px_size: f32) -> Option<FontFace> {
        let path = path?;
        match FontFace::load(path, px_size) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!(
                    "statusbar: failed to load fallback font {path:?}: {e} — its glyphs will render as tofu"
                );
                None
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
    pub fn draw_text(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: i32,
        y: i32,
        fg: Color,
        bg: Color,
    ) -> i32 {
        self.draw_text_alpha(canvas, text, x, y, fg, bg, 255)
    }

    /// Same as [`Self::draw_text`], but `bg_alpha` (`0-255`) is the
    /// background's own packed alpha -- needed so anti-aliased glyph edges
    /// blend correctly when the bar's background is translucent (mirrors
    /// `platform::Renderer`'s `bg_alpha` plumbing). `255` (opaque) is
    /// identical to [`Self::draw_text`].
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_alpha(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: i32,
        y: i32,
        fg: Color,
        bg: Color,
        bg_alpha: u8,
    ) -> i32 {
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
                    // `blit_glyph` (shared with `platform::Renderer`) adds the
                    // glyph's bearing to this pen origin and no-ops on a blank
                    // glyph, so no width/height guard is needed here.
                    blit_glyph(canvas, &f.atlas, &entry, pen_x, y, fg, bg, bg_alpha);
                    pen_x += entry.advance;
                }
                pen_x
            }
        }
    }

    /// Right-align `text` so its last glyph ends at `right_x`. Returns the
    /// measured width -- see [`Self::draw_text_right_alpha`].
    pub fn draw_text_right(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right_x: i32,
        y: i32,
        fg: Color,
        bg: Color,
    ) -> i32 {
        self.draw_text_right_alpha(canvas, text, right_x, y, fg, bg, 255)
    }

    /// [`Self::draw_text_right`] with an explicit background alpha -- see
    /// [`Self::draw_text_alpha`]. Returns the measured width, so callers
    /// that also need it (e.g. for hit-testing) don't have to pay for a
    /// second glyph walk via a separate `measure` call.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_right_alpha(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        right_x: i32,
        y: i32,
        fg: Color,
        bg: Color,
        bg_alpha: u8,
    ) -> i32 {
        let w = self.measure(text);
        self.draw_text_alpha(canvas, text, right_x - w, y, fg, bg, bg_alpha);
        w
    }
}

impl FaceRenderer {
    /// Look up (rasterizing + atlas-inserting on first use) the glyph entry
    /// for `codepoint`. `None` if the atlas is full.
    fn entry(&mut self, codepoint: u32) -> Option<GlyphEntry> {
        if let Some(e) = self.map.get(&codepoint) {
            return Some(*e);
        }
        // Consult the fallback faces, in order, only when the primary
        // genuinely lacks the glyph -- cheap cmap-only check, same pattern
        // as `platform::render::GlyphCache::entry`.
        let g = if self.face.has_glyph(codepoint) {
            self.face.rasterize(codepoint)
        } else if let Some(f) = self.fallbacks.iter_mut().find(|f| f.has_glyph(codepoint)) {
            f.rasterize(codepoint)
        } else {
            self.face.rasterize(codepoint)
        };
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


