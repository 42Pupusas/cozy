//! Text rendering for the bar.
//!
//! Either the embedded 8×16 bitmap font (no config, always available) or a
//! real TTF/OTF face loaded via `font`, rasterized into a growing glyph
//! atlas exactly like `platform::render`'s `GlyphCache` -- just decoupled
//! from the terminal's fixed-width cell grid, since bar text is
//! proportional-ish (still monospace-metric in practice, but drawn
//! glyph-by-glyph at each glyph's own advance rather than snapped to a
//! `cell_w` grid column).

use std::collections::HashMap;
use std::fs;

use font::{Atlas, FontFace, GlyphEntry};
use platform::GlyphBlit;
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
    /// Fallback faces (e.g. Noto Emoji, a Nerd Font for icon glyphs)
    /// consulted in order for codepoints `face` doesn't cover -- mirrors
    /// `platform::render`'s `GlyphCache`. Empty when no fallback fonts are
    /// configured (or all failed to load), in which case an uncovered
    /// codepoint rasterizes `face`'s own `.notdef`.
    fallbacks: Vec<FontFace>,
    /// Paths are retained instead of loading every fallback face during bar
    /// startup. Emoji/icon fonts can be much larger than the primary face and
    /// are only needed when a label actually contains one of their glyphs.
    fallback_paths: Vec<String>,
    fallback_px_size: f32,
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
        let Ok(data) = fs::read(path) else {
            return Self::load_with_fallbacks_from_data(path, &[], fallback_paths, px_size);
        };
        Self::load_with_fallbacks_from_data(path, &data, fallback_paths, px_size)
    }

    /// Load a primary face from bytes already read by the caller. This is
    /// useful when several UI text sizes share one font file: the file is
    /// read once while each size still gets its own metrics and atlas.
    #[must_use]
    pub fn load_with_fallbacks_from_bytes(
        path: &str,
        data: &[u8],
        fallback_paths: &[Option<&str>],
        px_size: f32,
    ) -> Self {
        Self::load_with_fallbacks_from_data(path, data, fallback_paths, px_size)
    }

    fn load_with_fallbacks_from_data(
        path: &str,
        data: &[u8],
        fallback_paths: &[Option<&str>],
        px_size: f32,
    ) -> Self {
        match FontFace::from_bytes(data.to_vec(), px_size) {
            Ok(face) => {
                let ascender = face.line.ascender;
                let cell_height = face.line.cell_height;
                // Keep startup responsive: rasterize glyphs on first use instead
                // of blocking the first frame while building every ASCII and
                // box-drawing glyph. The atlas still has ample room for the
                // statusbar's small, usually stable set of labels.
                let atlas = Atlas::new(2048, 2048);
                let map = HashMap::new();
                let fallback_paths = fallback_paths
                    .iter()
                    .filter_map(|path| (*path).map(str::to_owned))
                    .collect();
                Self::Face(Box::new(FaceRenderer {
                    face,
                    atlas,
                    map,
                    ascender,
                    cell_height,
                    fallbacks: Vec::new(),
                    fallback_paths,
                    fallback_px_size: px_size,
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
            // Float source, no lossless-widening path available (see the
            // font crate's raster.rs doc for the full float<->int cast
            // rationale) -- a glyph cell height is always tiny and
            // positive.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Self::Face(f) => f.cell_height.round().max(1.0) as i32,
        }
    }

    /// Cell-top offset of the shared face baseline. `TextRenderer` accepts a
    /// cell-top origin, while mixed-size faces need their ascenders aligned
    /// explicitly when composing one visual row.
    #[must_use]
    pub fn baseline_offset(&self) -> i32 {
        match self {
            Self::Bitmap => 12,
            // A loaded font's ascender is a small positive pixel metric. The
            // font parser represents metrics as `f32`, whereas drawing uses
            // signed pixel coordinates; text conversion gives this fallible
            // boundary an explicit saturation rule without an unchecked cast.
            Self::Face(f) => f
                .ascender
                .round()
                .max(0.0)
                .to_string()
                .parse::<i32>()
                .unwrap_or(i32::MAX),
        }
    }

    /// Y to pass as `draw_text`'s `y` so one line of text is vertically
    /// centered within a bar `bar_height` pixels thick.
    #[must_use]
    pub fn baseline_y(&self, bar_height: i32) -> i32 {
        ((bar_height - self.line_height()) / 2).max(0)
    }

    /// Populate the common dashboard glyphs before a renderer is handed to the
    /// UI thread. Font parsing and the first rasterization wave are both
    /// expensive; doing this on the loader thread avoids a several-second
    /// hitch when the configured face is first displayed.
    pub fn prewarm_common_glyphs(&mut self) {
        let common = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz%.-+_/():[]… ·";
        if let Self::Face(face) = self {
            for codepoint in common.chars().map(u32::from) {
                let _ = face.entry(codepoint);
            }
        }
    }

    /// Width in pixels `text` would occupy if drawn now (rasterizing any
    /// not-yet-cached glyphs as a side effect, same as `draw_text`).
    pub fn measure(&mut self, text: &str) -> i32 {
        match self {
            // A bar segment's rendered text is always short, well under
            // i32::MAX chars.
            Self::Bitmap => {
                i32::try_from(text.chars().count()).unwrap_or(i32::MAX) * bitmap_font::GLYPH_W
            }
            Self::Face(f) => {
                let mut w = 0.0f32;
                for ch in text.chars() {
                    // A glyph advance is always tiny (well under i16::MAX
                    // pixels), so narrowing through i16 first makes this
                    // widening to f32 provably lossless.
                    w += f.entry(ch as u32).map_or(0.0, |e| {
                        f32::from(i16::try_from(e.advance).unwrap_or(i16::MAX))
                    });
                }
                // Float source, no lossless-widening path available (see
                // the font crate's raster.rs doc) -- a measured text width
                // is always tiny and positive.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let width = w.round() as i32;
                width
            }
        }
    }

    /// Draw `text` left-to-right with its top-left (bitmap font) / baseline
    /// row (face font) at `(x, y)`, over an already-painted `bg` (needed for
    /// anti-aliased edge blending on the face path -- the bitmap path
    /// ignores it, being 1-bit). Returns the x just past the last glyph.
    ///
    /// The background alpha is taken from the canvas's own surface format, so
    /// text drawn on a translucent window blends its anti-aliased edges
    /// correctly without every call site having to thread the window opacity
    /// through. This used to hardcode `255`, which on a translucent surface
    /// packed each edge pixel with a zero alpha byte -- a transparent pinhole
    /// around every glyph. Callers that need an alpha *other* than their
    /// surface's (a translucent widget on an opaque window, say) still say so
    /// explicitly via [`Self::draw_text_alpha`].
    pub fn draw_text(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        x: i32,
        y: i32,
        fg: Color,
        bg: Color,
    ) -> i32 {
        let bg_alpha = canvas.format().bg_alpha();
        self.draw_text_alpha(canvas, text, x, y, fg, bg, bg_alpha)
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
                // A bar segment's rendered text is always short, well
                // under i32::MAX chars.
                x + i32::try_from(text.chars().count()).unwrap_or(i32::MAX) * bitmap_font::GLYPH_W
            }
            Self::Face(f) => {
                let mut pen_x = x;
                for ch in text.chars() {
                    let Some(entry) = f.entry(ch as u32) else {
                        continue;
                    };
                    // `Canvas::blit_glyph` (shared with `platform::Renderer`) adds the
                    // glyph's bearing to this pen origin and no-ops on a blank
                    // glyph, so no width/height guard is needed here.
                    canvas.blit_glyph(&f.atlas, &entry, pen_x, y, fg, bg, bg_alpha);
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
        // Surface-derived alpha, as in `draw_text`.
        let bg_alpha = canvas.format().bg_alpha();
        self.draw_text_right_alpha(canvas, text, right_x, y, fg, bg, bg_alpha)
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
        } else {
            self.load_fallbacks_until_match(codepoint);
            if let Some(f) = self.fallbacks.iter_mut().find(|f| f.has_glyph(codepoint)) {
                f.rasterize(codepoint)
            } else {
                self.face.rasterize(codepoint)
            }
        };
        // Float source, no lossless-widening path available (see the font
        // crate's raster.rs doc for the full float<->int cast rationale) --
        // glyph bearing/advance are always tiny pixel offsets.
        #[allow(clippy::cast_possible_truncation)]
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

    /// Load configured fallback faces only when the primary face misses a
    /// glyph. Most bars never use emoji or icon glyphs, so deferring this work
    /// keeps the first frame independent of those large font files.
    fn load_fallbacks_until_match(&mut self, codepoint: u32) {
        while !self.fallbacks.iter().any(|face| face.has_glyph(codepoint)) {
            let Some(path) = self.fallback_paths.first().cloned() else {
                break;
            };
            self.fallback_paths.remove(0);
            if let Some(face) = Self::load_fallback(&path, self.fallback_px_size) {
                self.fallbacks.push(face);
            }
        }
    }

    /// Load one fallback face on demand. Diagnoses (but doesn't abort on) a
    /// bad path, same treatment as the primary face's own failure handling.
    /// Lives here — on the sole consumer of fallback faces — rather than on
    /// the owning `TextRenderer`, so ownership flows parent -> child.
    fn load_fallback(path: &str, px_size: f32) -> Option<FontFace> {
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
}
