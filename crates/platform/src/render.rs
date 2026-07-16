#![allow(clippy::cast_possible_truncation)]
//! Software terminal renderer: blit grid cells into a `u32` pixel buffer.
//!
//! Two glyph sources are supported, chosen at startup by [`Renderer::new`]:
//!
//! * **Bitmap fallback** — the embedded 8×16 [`crate::font`] PSF bitmap,
//!   ASCII-only. Used when no `[font] path` is configured, or the configured
//!   file fails to load/parse.
//! * **TTF/OTF face** — a real font loaded via the `font` crate, rasterized
//!   on demand into a growing glyph atlas (anti-aliased 8-bit alpha,
//!   any Unicode codepoint the font covers — CJK, box-drawing, emoji, …).
//!
//! Color mapping: converts 256-palette indices to 24-bit RGB using the standard
//! xterm color table; true-color cells pass through directly.

use std::collections::HashMap;

use grid::{Attrs, Cell, Color, DirtySnapshot};

use crate::draw::{Canvas, Color as DrawColor};
use crate::font::{FIRST, GLYPH_H, GLYPH_W, LAST};

pub use crate::font::FONT;

// ── xterm 256-color palette ──────────────────────────────────────────────────

/// xterm 256-color palette: indices 0–255 → (r, g, b).
/// We generate the classic xterm table at compile time.
static PALETTE: [(u8, u8, u8); 256] = {
    let mut t = [(0u8, 0u8, 0u8); 256];
    // 0–7: standard colors
    t[0] = (0, 0, 0);
    t[1] = (128, 0, 0);
    t[2] = (0, 128, 0);
    t[3] = (128, 128, 0);
    t[4] = (0, 0, 128);
    t[5] = (128, 0, 128);
    t[6] = (0, 128, 128);
    t[7] = (192, 192, 192);
    // 8–15: bright colors
    t[8] = (128, 128, 128);
    t[9] = (255, 0, 0);
    t[10] = (0, 255, 0);
    t[11] = (255, 255, 0);
    t[12] = (0, 0, 255);
    t[13] = (255, 0, 255);
    t[14] = (0, 255, 255);
    t[15] = (255, 255, 255);
    // 16–231: 6×6×6 color cube
    let mut i = 16usize;
    let mut r = 0u8;
    while r < 6 {
        let mut g = 0u8;
        while g < 6 {
            let mut b = 0u8;
            while b < 6 {
                let rv = if r == 0 { 0 } else { r * 40 + 55 };
                let gv = if g == 0 { 0 } else { g * 40 + 55 };
                let bv = if b == 0 { 0 } else { b * 40 + 55 };
                t[i] = (rv, gv, bv);
                i += 1;
                b += 1;
            }
            g += 1;
        }
        r += 1;
    }
    // 232–255: grayscale ramp
    let mut j = 0u8;
    while j < 24 {
        let v = j * 10 + 8;
        t[232 + j as usize] = (v, v, v);
        j += 1;
    }
    t
};

/// Built-in default fg/bg (used when no config theme is supplied).
const DEFAULT_FG: DrawColor = DrawColor::rgb(0xcc, 0xcc, 0xcc);
const DEFAULT_BG: DrawColor = DrawColor::rgb(0x1e, 0x1e, 0x2e);

/// Resolved color theme: the 16 ANSI colors plus default fg/bg. Indices 16–255
/// fall through to the static xterm cube. Built from a [`config::Palette`] via
/// [`Theme::from_palette`], or [`Theme::builtin`] for the hard-coded default.
#[derive(Clone)]
pub struct Theme {
    /// ANSI 0–15 overrides (RGB).
    ansi: [(u8, u8, u8); 16],
    fg: DrawColor,
    bg: DrawColor,
}

impl Theme {
    /// The built-in dark theme (xterm ANSI colors + grey-on-charcoal default).
    #[must_use]
    pub fn builtin() -> Self {
        let mut ansi = [(0u8, 0u8, 0u8); 16];
        let mut i = 0;
        while i < 16 {
            ansi[i] = PALETTE[i];
            i += 1;
        }
        Self {
            ansi,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
        }
    }

    /// Build a theme from a parsed config palette.
    #[must_use]
    pub fn from_palette(p: &config::Palette) -> Self {
        let mut ansi = [(0u8, 0u8, 0u8); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            let c = p.ansi(i as u8);
            *slot = (c.r, c.g, c.b);
        }
        Self {
            ansi,
            fg: DrawColor::rgb(p.foreground.r, p.foreground.g, p.foreground.b),
            bg: DrawColor::rgb(p.background.r, p.background.g, p.background.b),
        }
    }

    /// The theme background as a packed XRGB8888 word (backbuffer clear color).
    #[must_use]
    pub fn bg_xrgb(&self) -> u32 {
        self.bg.to_xrgb8888()
    }

    fn resolve(&self, color: Color, default: DrawColor) -> DrawColor {
        match color {
            Color::Default => default,
            Color::Indexed(n) if n < 16 => {
                let (r, g, b) = self.ansi[n as usize];
                DrawColor::rgb(r, g, b)
            }
            Color::Indexed(n) => {
                let (r, g, b) = PALETTE[n as usize];
                DrawColor::rgb(r, g, b)
            }
            Color::Rgb(r, g, b) => DrawColor::rgb(r, g, b),
        }
    }

    fn effective(&self, attrs: &Attrs) -> (DrawColor, DrawColor) {
        let fg = self.resolve(attrs.fg, self.fg);
        let bg = self.resolve(attrs.bg, self.bg);
        if attrs.inverse { (bg, fg) } else { (fg, bg) }
    }
}

// ── Glyph sources ─────────────────────────────────────────────────────────────

/// A real TTF/OTF face plus its glyph atlas, growing on demand as new
/// codepoints are drawn. Pre-seeded with printable ASCII + box-drawing so the
/// common case never rasterizes at paint time.
struct GlyphCache {
    face: font::FontFace,
    atlas: font::Atlas,
    map: HashMap<u32, font::GlyphEntry>,
    /// Distance from the top of the cell to the baseline, in pixels.
    ascender: f32,
}

impl GlyphCache {
    fn new(mut face: font::FontFace) -> Self {
        let ascender = face.line.ascender;
        let (atlas, map) = face.build_ascii_atlas(2048);
        Self {
            face,
            atlas,
            map,
            ascender,
        }
    }

    /// Look up (rasterizing + atlas-inserting on first use) the glyph entry
    /// for `codepoint`. `None` if the atlas is full (extremely large glyph
    /// sets) — the caller then just paints background for that cell.
    fn entry(&mut self, codepoint: u32) -> Option<font::GlyphEntry> {
        if let Some(e) = self.map.get(&codepoint) {
            return Some(*e);
        }
        // `self.face` and `self.atlas` are disjoint fields, so this doesn't
        // fight the borrow checker despite the mutable + immutable overlap.
        let g = self.face.rasterize(codepoint);
        let entry = self.atlas.insert(
            &g.bitmap,
            g.metrics.bearing_x as i32,
            g.metrics.bearing_y as i32,
            g.metrics.advance as i32,
        );
        if let Some(e) = entry {
            self.map.insert(codepoint, e);
        }
        entry
    }
}

enum GlyphSource {
    /// The embedded 8×16 ASCII-only bitmap font (see [`crate::font`]).
    Bitmap,
    /// A loaded TTF/OTF face rasterized into a growing atlas. Boxed to keep
    /// the `Bitmap` variant (the common case) from paying for the atlas's size.
    Face(Box<GlyphCache>),
}

/// Owns the color theme and glyph source, and knows how to blit grid cells
/// into a [`Canvas`]. Replaces the old free-standing `Canvas::blit_cell` /
/// `blit_snapshot` methods so the (now dynamic, font-dependent) cell size and
/// the glyph cache have somewhere to live between frames.
pub struct Renderer {
    theme: Theme,
    glyphs: GlyphSource,
    /// Resolved terminal cell size in pixels — `8×16` for the bitmap
    /// fallback, or derived from the loaded face's metrics.
    cell_w: i32,
    cell_h: i32,
}

impl Renderer {
    /// Build a renderer using only the embedded bitmap font (no TTF/OTF
    /// loading). Used by tests and as the fallback path of [`Self::new`].
    #[must_use]
    pub fn builtin(theme: Theme) -> Self {
        Self {
            theme,
            glyphs: GlyphSource::Bitmap,
            cell_w: GLYPH_W,
            cell_h: GLYPH_H,
        }
    }

    /// Build a renderer from config: tries to load `font_cfg.path` at
    /// `font_cfg.size` pixels; falls back to the embedded bitmap font (with a
    /// diagnostic on stderr) if no path is configured or loading fails.
    #[must_use]
    pub fn new(theme: Theme, font_cfg: &config::FontConfig) -> Self {
        let Some(path) = &font_cfg.path else {
            return Self::builtin(theme);
        };
        // font.size is validated to 1..=256 in config; well within f32's exact range.
        #[allow(clippy::cast_precision_loss)]
        let px_size = font_cfg.size as f32;
        match font::FontFace::load(path, px_size) {
            Ok(face) => {
                let cell_w = face.cell_width.round().max(1.0) as i32;
                let cell_h = face.line.cell_height.round().max(1.0) as i32;
                Self {
                    theme,
                    glyphs: GlyphSource::Face(Box::new(GlyphCache::new(face))),
                    cell_w,
                    cell_h,
                }
            }
            Err(e) => {
                eprintln!(
                    "platform: failed to load font {path:?}: {e} — falling back to embedded bitmap font"
                );
                Self::builtin(theme)
            }
        }
    }

    /// Terminal cell width in pixels.
    #[must_use]
    pub const fn cell_w(&self) -> i32 {
        self.cell_w
    }

    /// Terminal cell height in pixels.
    #[must_use]
    pub const fn cell_h(&self) -> i32 {
        self.cell_h
    }

    /// The theme background as a packed XRGB8888 word (backbuffer clear color).
    #[must_use]
    pub fn bg_xrgb(&self) -> u32 {
        self.theme.bg_xrgb()
    }

    /// Blit a single cell into the canvas at cell column/row `cx`/`cy`.
    pub fn blit_cell(&mut self, canvas: &mut Canvas<'_>, cx: u16, cy: u16, cell: &Cell) {
        // Spacer cells are the trailing (invisible) half of a wide glyph: the
        // lead cell paints both columns already, so drawing here would just
        // stamp a redundant blank over it (harmless but wasteful) -- skip.
        if cell.spacer {
            return;
        }

        let (fg, bg) = self.theme.effective(&cell.attrs);
        let fg_px = fg.to_xrgb8888();
        let bg_px = bg.to_xrgb8888();
        let ch = if cell.attrs.invisible { ' ' } else { cell.ch };

        match &mut self.glyphs {
            GlyphSource::Bitmap => {
                Self::blit_cell_bitmap(canvas, cx, cy, cell.wide, ch, fg_px, bg_px);
            }
            GlyphSource::Face(cache) => {
                Self::blit_cell_face(
                    canvas,
                    cache,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    cell.wide,
                    ch,
                    fg,
                    bg,
                );
            }
        }
    }

    /// Bitmap-font path: identical to the pre-M9 renderer (1-bit glyph, no AA).
    fn blit_cell_bitmap(
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        wide: bool,
        ch: char,
        fg_px: u32,
        bg_px: u32,
    ) {
        let buf_width = canvas.width() as u32;
        let pixels = canvas.pixels_mut();

        let px0 = u32::from(cx) * GLYPH_W as u32;
        let py0 = u32::from(cy) * GLYPH_H as u32;
        let cell_cols: u32 = if wide { 2 } else { 1 };

        let glyph: Option<&[u8; 16]> = if ch >= FIRST as char && ch <= LAST as char {
            let idx = ch as usize - FIRST as usize;
            FONT.get(idx)
        } else {
            None
        };

        for row in 0..GLYPH_H as u32 {
            let py = py0 + row;
            let row_mask: u8 = glyph.map_or(0xff, |g| g[row as usize]);
            for col in 0..(GLYPH_W as u32 * cell_cols) {
                let px = px0 + col;
                // See the wide-snapshot clip test below: without this a stale
                // over-wide snapshot would wrap into the next row.
                if px >= buf_width {
                    continue;
                }
                let bit = if col < GLYPH_W as u32 {
                    (row_mask >> (7 - col)) & 1
                } else {
                    0
                };
                let pixel = if bit != 0 { fg_px } else { bg_px };
                let idx = (py * buf_width + px) as usize;
                if idx < pixels.len() {
                    pixels[idx] = pixel;
                }
            }
        }
    }

    /// Real-font path: fill the cell background, then alpha-blend the
    /// rasterized (possibly multi-cell-wide, anti-aliased) glyph over it.
    #[allow(clippy::too_many_arguments)]
    fn blit_cell_face(
        canvas: &mut Canvas<'_>,
        cache: &mut GlyphCache,
        cx: u16,
        cy: u16,
        cell_w: i32,
        cell_h: i32,
        wide: bool,
        ch: char,
        fg: DrawColor,
        bg: DrawColor,
    ) {
        let buf_width = canvas.width() as u32;
        let fg_px = fg.to_xrgb8888();
        let bg_px = bg.to_xrgb8888();
        let px0 = u32::from(cx) * cell_w as u32;
        let py0 = u32::from(cy) * cell_h as u32;
        let cell_cols: u32 = if wide { 2 } else { 1 };

        {
            let pixels = canvas.pixels_mut();
            for row in 0..cell_h as u32 {
                let py = py0 + row;
                for col in 0..(cell_w as u32 * cell_cols) {
                    let px = px0 + col;
                    if px >= buf_width {
                        continue;
                    }
                    let idx = (py * buf_width + px) as usize;
                    if idx < pixels.len() {
                        pixels[idx] = bg_px;
                    }
                }
            }
        }

        let Some(entry) = cache.entry(ch as u32) else {
            return;
        };
        if entry.width == 0 || entry.height == 0 {
            return; // blank glyph (space etc.) — background is enough.
        }

        let baseline_y = py0 as i64 + cache.ascender.round() as i64;
        let glyph_x0 = px0 as i64 + i64::from(entry.bearing_x);
        let glyph_y0 = baseline_y - i64::from(entry.bearing_y);

        let pixels = canvas.pixels_mut();
        for row in 0..entry.height {
            let py = glyph_y0 + i64::from(row);
            if py < 0 {
                continue;
            }
            let py = py as u32;
            for col in 0..entry.width {
                let px = glyph_x0 + i64::from(col);
                if px < 0 || px as u32 >= buf_width {
                    continue;
                }
                let px = px as u32;
                let a = cache.atlas.data
                    [((entry.y + row) * cache.atlas.width + (entry.x + col)) as usize];
                if a == 0 {
                    continue;
                }
                let idx = (py * buf_width + px) as usize;
                let Some(dst) = pixels.get_mut(idx) else {
                    continue;
                };
                *dst = if a == 255 { fg_px } else { blend(bg, fg, a) };
            }
        }
    }

    /// Blit every dirty row in `snap` into the canvas.
    pub fn blit_snapshot(&mut self, canvas: &mut Canvas<'_>, snap: &DirtySnapshot) {
        // Rows of the glyph grid that fit in this buffer. A stale snapshot from a
        // taller geometry may carry rows past the bottom; clip them rather than let
        // them paint into the wrong pixels.
        let buf_width = canvas.width() as u32;
        let buf_rows = (canvas.pixels_mut().len() as u32 / buf_width.max(1)) / self.cell_h as u32;
        for row in &snap.rows {
            if u32::from(row.row) >= buf_rows {
                continue;
            }
            for (col, cell) in row.cells.iter().enumerate() {
                self.blit_cell(canvas, col as u16, row.row, cell);
            }
        }
    }
}

/// Linear-interpolate `from` → `to` by `alpha/255`, packed as XRGB8888.
fn blend(from: DrawColor, to: DrawColor, alpha: u8) -> u32 {
    let a = u32::from(alpha);
    let r = (u32::from(from.r) * (255 - a) + u32::from(to.r) * a) / 255;
    let g = (u32::from(from.g) * (255 - a) + u32::from(to.g) * a) / 255;
    let b = (u32::from(from.b) * (255 - a) + u32::from(to.b) * a) / 255;
    (r << 16) | (g << 8) | b
}

#[cfg(test)]
mod tests {
    use super::*;
    use grid::{DirtyRow, DirtySnapshot};

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            attrs: Attrs::default(),
            wide: false,
            spacer: false,
        }
    }

    #[test]
    fn stale_wide_snapshot_does_not_bleed_into_next_row() {
        // Backbuffer sized for a NARROW grid: 2 cells wide, 2 rows tall.
        let bw = 2 * GLYPH_W as u32;
        let bh = 2 * GLYPH_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];

        // A STALE snapshot from a wider geometry: row 0 has 5 cells. Without
        // the px < buf_width clip, cells 2..5 would wrap into row 0's lower
        // pixel lines / row 1. We assert row 1's cell region stays untouched.
        let snap = DirtySnapshot {
            cols: 5,
            rows: vec![DirtyRow {
                row: 0,
                cells: (0..5).map(|_| cell('M')).collect(),
            }],
        };
        let theme = Theme::builtin();
        let bg = theme.bg_xrgb();
        let mut renderer = Renderer::builtin(theme);
        let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
        renderer.blit_snapshot(&mut canvas, &snap);

        // The second glyph row (pixels y in [GLYPH_H, 2*GLYPH_H)) must be all
        // background — nothing from the over-wide row 0 may have leaked there.
        for y in GLYPH_H as u32..bh {
            for x in 0..bw {
                let px = pixels[(y * bw + x) as usize];
                assert_eq!(
                    px, 0,
                    "row 1 pixel ({x},{y}) was painted: {px:#x} (bg={bg:#x})"
                );
            }
        }
    }

    #[test]
    fn stale_tall_snapshot_row_is_clipped() {
        // Backbuffer for 2 rows; a stale snapshot references row 3.
        let bw = 2 * GLYPH_W as u32;
        let bh = 2 * GLYPH_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];
        let snap = DirtySnapshot {
            cols: 2,
            rows: vec![DirtyRow {
                row: 3,
                cells: vec![cell('M'), cell('M')],
            }],
        };
        let mut renderer = Renderer::builtin(Theme::builtin());
        let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
        renderer.blit_snapshot(&mut canvas, &snap);
        // Nothing should have been drawn at all.
        assert!(
            pixels.iter().all(|&p| p == 0),
            "off-bottom row 3 leaked into buffer"
        );
    }

    #[test]
    fn builtin_renderer_uses_bitmap_cell_size() {
        let r = Renderer::builtin(Theme::builtin());
        assert_eq!(r.cell_w(), GLYPH_W);
        assert_eq!(r.cell_h(), GLYPH_H);
    }

    #[test]
    fn blend_alpha_zero_is_background_alpha_255_is_foreground() {
        let bg = DrawColor::rgb(0, 0, 0);
        let fg = DrawColor::rgb(255, 255, 255);
        assert_eq!(blend(bg, fg, 0), bg.to_xrgb8888());
        assert_eq!(blend(bg, fg, 255), fg.to_xrgb8888());
    }
}
