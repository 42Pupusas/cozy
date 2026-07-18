#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
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

use grid::{Attrs, Cell, Color, DirtySnapshot, ScrollBand};

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
        if attrs.inverse() { (bg, fg) } else { (fg, bg) }
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
            g.metrics.bearing_x.round() as i32,
            font::FontFace::cell_bearing_y(self.ascender, g.metrics.bearing_y),
            g.metrics.advance.round() as i32,
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
    /// fallback, or derived from the loaded face's metrics. Already includes
    /// `output_scale` (i.e. these are physical/buffer pixels, not logical
    /// ones), so callers never need to multiply by scale again.
    cell_w: i32,
    cell_h: i32,
    /// The Wayland output's integer buffer scale this renderer was built for
    /// (1 = standard DPI, 2 = HiDPI @2x, ...). The font was rasterized at
    /// `font_cfg.px_size() * output_scale` so glyphs are crisp at the
    /// display's native pixel density instead of being upscaled/blurred by
    /// the compositor. See `TermWindow::new`, which reads this from the
    /// bound `wl_output` and both passes it here and calls
    /// `wl_surface.set_buffer_scale` with the same value.
    output_scale: i32,
    /// Alpha applied to *pure background* pixels (no glyph coverage), `0–255`.
    /// `255` (the default) means fully opaque, and every packed pixel is then
    /// byte-identical to the pre-transparency `to_xrgb8888()` output — this
    /// only changes behavior when a window opacity `< 1.0` is configured.
    /// Any pixel with glyph coverage (even a faint anti-aliased edge) is
    /// always packed fully opaque, matching Alacritty's default "background
    /// glass, text solid" look (`transparent_background_colors = false`).
    bg_alpha: u8,
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
            output_scale: 1,
            bg_alpha: 255,
        }
    }

    /// Set the window background opacity, `0.0` (fully transparent) to `1.0`
    /// (opaque). Only affects pure-background pixels — glyph pixels are
    /// always packed fully opaque (see [`Self::bg_alpha`]). Values outside
    /// `[0.0, 1.0]` are clamped; `config::WindowConfig::opacity` is already
    /// validated to that range, so this is just defense in depth.
    #[must_use]
    pub fn with_opacity(mut self, opacity: f64) -> Self {
        self.bg_alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        self
    }

    /// Whether this renderer's window needs an alpha-capable Wayland buffer
    /// format (`Argb8888`) rather than the cheaper opaque `Xrgb8888`.
    #[must_use]
    pub const fn needs_alpha(&self) -> bool {
        self.bg_alpha < 255
    }

    /// Build a renderer from config: tries to load `font_cfg.path` at
    /// `font_cfg.size` pixels; falls back to the embedded bitmap font (with a
    /// diagnostic on stderr) if no path is configured or loading fails.
    ///
    /// Equivalent to `Self::with_scale(theme, font_cfg, 1)` — kept as the
    /// zero-argument entry point for callers (tests, anything not talking to
    /// a real scaled Wayland output) that don't care about HiDPI.
    #[must_use]
    pub fn new(theme: Theme, font_cfg: &config::FontConfig) -> Self {
        Self::with_scale(theme, font_cfg, 1)
    }

    /// Build a renderer from config, rasterizing the font at
    /// `font_cfg.px_size() * output_scale` physical pixels instead of the
    /// naive logical size.
    ///
    /// Without this, a window on a HiDPI/scaled Wayland output (`wl_output`
    /// advertising `scale > 1`) would render its buffer at 1x and rely on
    /// the *compositor* to upscale it to fill the physical pixels -- which
    /// is exactly what a bilinear upscale of anti-aliased glyph edges looks
    /// like: soft/fuzzy text, most visible on a font with fine strokes (the
    /// symptom that surfaced switching Inconsolata -> Noto Sans Mono). The
    /// caller (`TermWindow::new`) matches this by calling
    /// `wl_surface.set_buffer_scale(output_scale)`, which tells the
    /// compositor our buffer is already at native density and needs no
    /// resampling; `cell_w`/`cell_h` here come out already in physical
    /// pixels so the window-sizing math downstream doesn't need to know
    /// about scale at all.
    #[must_use]
    pub fn with_scale(theme: Theme, font_cfg: &config::FontConfig, output_scale: i32) -> Self {
        let output_scale = output_scale.max(1);
        let Some(path) = &font_cfg.path else {
            let mut r = Self::builtin(theme);
            r.output_scale = output_scale;
            r.cell_w *= output_scale;
            r.cell_h *= output_scale;
            return r;
        };
        // font.size is a point size (like Alacritty); convert to pixels at
        // the standard 96 DPI, then scale up to the output's physical pixel
        // density so a given point size renders crisp (not upscaled) at
        // whatever HiDPI factor the display uses.
        let px_size = font_cfg.px_size() * output_scale as f32;
        match font::FontFace::load(path, px_size) {
            Ok(face) => {
                let cell_w = face.cell_width.round().max(1.0) as i32;
                let cell_h = face.line.cell_height.round().max(1.0) as i32;
                Self {
                    theme,
                    glyphs: GlyphSource::Face(Box::new(GlyphCache::new(face))),
                    cell_w,
                    cell_h,
                    output_scale,
                    bg_alpha: 255,
                }
            }
            Err(e) => {
                eprintln!(
                    "platform: failed to load font {path:?}: {e} — falling back to embedded bitmap font"
                );
                let mut r = Self::builtin(theme);
                r.output_scale = output_scale;
                r.cell_w *= output_scale;
                r.cell_h *= output_scale;
                r
            }
        }
    }

    /// The Wayland output buffer scale this renderer was built for (see
    /// [`Self::with_scale`]). `1` for [`Self::new`]/[`Self::builtin`].
    #[must_use]
    pub const fn output_scale(&self) -> i32 {
        self.output_scale
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

    /// The theme background as a packed XRGB8888 word (backbuffer clear
    /// color), ignoring window opacity. Kept byte-identical to before M10's
    /// transparency support for any caller that only ever ran opaque
    /// windows; use [`Self::bg_packed`] for a backbuffer fill that's correct
    /// under a configured `[window] opacity`.
    #[must_use]
    pub fn bg_xrgb(&self) -> u32 {
        self.theme.bg_xrgb()
    }

    /// The theme background, packed for whatever this renderer's buffer
    /// format actually is: plain XRGB8888 (top byte ignored, identical to
    /// [`Self::bg_xrgb`]) when opaque, or premultiplied ARGB8888 at
    /// [`Self::needs_alpha`]'s alpha when a window opacity `< 1.0` is
    /// configured. Callers that fill/clear a whole backbuffer (initial
    /// alloc, resize-carry) should use this, not `bg_xrgb`, so newly exposed
    /// area matches the translucent look instead of flashing opaque.
    #[must_use]
    pub fn bg_packed(&self) -> u32 {
        Self::pack_bg(self.theme.bg, self.bg_alpha)
    }

    /// Pack a background color at `bg_alpha`: plain XRGB8888 when fully
    /// opaque (byte-identical to the pre-transparency renderer), or
    /// premultiplied ARGB8888 otherwise.
    fn pack_bg(bg: DrawColor, bg_alpha: u8) -> u32 {
        if bg_alpha == 255 {
            bg.to_xrgb8888()
        } else {
            bg.premultiplied_argb8888(bg_alpha)
        }
    }

    /// Blit a single cell into the canvas at cell column/row `cx`/`cy`.
    pub fn blit_cell(&mut self, canvas: &mut Canvas<'_>, cx: u16, cy: u16, cell: &Cell) {
        self.blit_cell_inner(canvas, cx, cy, cell, true);
    }

    /// Core of [`Self::blit_cell`]. `fill_bg` controls whether the cell paints
    /// its own background rectangle first: `true` for standalone single-cell
    /// callers (cursor imprint restore, status line), `false` when
    /// [`Self::blit_row`] has already laid down the background for the whole
    /// row in one batched pass and only the glyph is still owed.
    fn blit_cell_inner(
        &mut self,
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        cell: &Cell,
        fill_bg: bool,
    ) {
        // Spacer cells are the trailing (invisible) half of a wide glyph: the
        // lead cell paints both columns already, so drawing here would just
        // stamp a redundant blank over it (harmless but wasteful) -- skip.
        if cell.spacer {
            return;
        }

        let (fg, bg) = self.theme.effective(&cell.attrs);
        // Glyph ink is always fully opaque, regardless of window opacity —
        // matching Alacritty's default "translucent glass, solid text" look.
        // `premultiplied_argb8888(255)` degenerates to plain packed RGB (top
        // byte 0xff) in the opaque case, so this is a no-op split from
        // `to_xrgb8888()` only once `needs_alpha()` is true.
        let fg_px = if self.bg_alpha == 255 {
            fg.to_xrgb8888()
        } else {
            fg.premultiplied_argb8888(255)
        };
        let bg_px = Self::pack_bg(bg, self.bg_alpha);
        let ch = if cell.attrs.invisible() { ' ' } else { cell.ch };

        match &mut self.glyphs {
            GlyphSource::Bitmap => {
                Self::blit_cell_bitmap(canvas, cx, cy, cell.wide, ch, fg_px, bg_px, fill_bg);
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
                    self.bg_alpha,
                    fill_bg,
                );
            }
        }
    }

    /// Blit one dirty row, **batching the background fill** across runs of
    /// cells that share a packed background color into a single wide `fill`
    /// per scanline, then painting only the glyphs on top.
    ///
    /// The old per-cell path filled each cell's background separately -- for a
    /// mostly-blank row (the common case under a scroll flood) that was one
    /// narrow `fill` per cell per scanline, ~173 tiny fills per scanline where
    /// one wide fill suffices. Here pass 1 coalesces equal-background columns
    /// (wide-glyph spacer columns inherit their lead's background, matching the
    /// old two-column fill) and pass 2 skips blank cells entirely -- a space
    /// or invisible cell owes nothing once its background is down, so the
    /// glyph pass (and its per-cell `theme.effective`) is never entered for
    /// it.
    fn blit_row(&mut self, canvas: &mut Canvas<'_>, cy: u16, cells: &[Cell]) {
        let cell_w = self.cell_w as u32;
        let cell_h = self.cell_h as u32;
        let buf_width = canvas.width() as u32;
        let py0 = u32::from(cy) * cell_h;

        // ── Pass 1: batched background ──────────────────────────────────────
        {
            let mut c = 0usize;
            while c < cells.len() {
                let (_, bg) = self.theme.effective(&cells[c].attrs);
                let base_bg = Self::pack_bg(bg, self.bg_alpha);
                let start = c;
                c += 1;
                // Extend the run while the background matches. A spacer column
                // (trailing half of a wide glyph) always belongs to its lead's
                // run, so it never breaks it -- this reproduces the old path,
                // where the wide lead filled both of its columns' backgrounds
                // and the spacer filled nothing.
                while c < cells.len() {
                    let next_bg = if cells[c].spacer {
                        base_bg
                    } else {
                        let (_, bg) = self.theme.effective(&cells[c].attrs);
                        Self::pack_bg(bg, self.bg_alpha)
                    };
                    if next_bg != base_bg {
                        break;
                    }
                    c += 1;
                }

                let px_start = start as u32 * cell_w;
                if px_start >= buf_width {
                    continue;
                }
                let px_end = (c as u32 * cell_w).min(buf_width);
                let w = (px_end - px_start) as usize;
                let pixels = canvas.pixels_mut();
                for row in 0..cell_h {
                    let py = py0 + row;
                    let base = (py * buf_width + px_start) as usize;
                    if let Some(slice) = pixels.get_mut(base..base + w) {
                        slice.fill(base_bg);
                    }
                }
            }
        }

        // ── Pass 2: glyphs only ─────────────────────────────────────────────
        for (col, cell) in cells.iter().enumerate() {
            // Blank cells owe nothing once the background is down. Skipping
            // them here also skips their `theme.effective` + glyph lookup --
            // the whole point on a mostly-space screen. `invisible()` forces a
            // blank glyph, so it is blank too.
            if cell.spacer || cell.ch == ' ' || cell.attrs.invisible() {
                continue;
            }
            self.blit_cell_inner(canvas, col as u16, cy, cell, false);
        }
    }

    /// Bitmap-font path: 1-bit glyph, no AA.
    ///
    /// Row-batched like [`Self::blit_cell_face`]: the clipped scanline width
    /// is computed once per cell (not once per pixel), each row is
    /// background-filled with a single slice `fill`, and ink is stamped only
    /// where the glyph's bitmask has set bits -- a space (the overwhelmingly
    /// common cell) does one `fill` per row and nothing else. Previously this
    /// did a per-pixel bounds check + bit test + index multiply -- a hot
    /// path `perf record` on the `blit` bench showed dominating render time.
    ///
    /// Unknown/non-ASCII chars fall back to a solid ink block (`mask =
    /// 0xff`), matching the pre-M9 renderer's "tofu" glyph -- this is
    /// deliberately different from [`Canvas::draw_char`], which leaves
    /// unknown chars blank.
    #[allow(clippy::too_many_arguments)]
    fn blit_cell_bitmap(
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        wide: bool,
        ch: char,
        fg_px: u32,
        bg_px: u32,
        fill_bg: bool,
    ) {
        let buf_width = canvas.width() as u32;
        let px0 = u32::from(cx) * GLYPH_W as u32;
        let py0 = u32::from(cy) * GLYPH_H as u32;
        let cell_cols: u32 = if wide { 2 } else { 1 };
        let cell_px_w = GLYPH_W as u32 * cell_cols;

        // See the wide-snapshot clip test below: without this a stale
        // over-wide snapshot would wrap into the next row.
        let clipped_w = if px0 >= buf_width {
            0
        } else {
            cell_px_w.min(buf_width - px0)
        };
        if clipped_w == 0 {
            return;
        }
        // The bitmap glyph itself is always exactly GLYPH_W wide (no
        // double-width bitmap glyphs) -- a wide cell's second half is pure
        // background, so ink is never stamped past this.
        let glyph_w = clipped_w.min(GLYPH_W as u32);

        let glyph: Option<&[u8; 16]> = if ch >= FIRST as char && ch <= LAST as char {
            let idx = ch as usize - FIRST as usize;
            FONT.get(idx)
        } else {
            None
        };

        let pixels = canvas.pixels_mut();
        for row in 0..GLYPH_H as u32 {
            let py = py0 + row;
            let row_start = (py * buf_width + px0) as usize;
            let row_end = row_start + clipped_w as usize;
            let Some(row_slice) = pixels.get_mut(row_start..row_end) else {
                continue;
            };
            if fill_bg {
                row_slice.fill(bg_px);
            }

            let mask: u8 = glyph.map_or(0xff, |g| g[row as usize]);
            if mask == 0 {
                continue; // already background-filled above.
            }
            if mask == 0xff && glyph_w == GLYPH_W as u32 {
                // Solid row (blank-font fallback, or a glyph row that
                // happens to be fully inked) -- one fill instead of 8 bit
                // tests.
                row_slice[..glyph_w as usize].fill(fg_px);
                continue;
            }
            for col in 0..glyph_w {
                if mask & (0x80 >> col) != 0 {
                    row_slice[col as usize] = fg_px;
                }
            }
        }
    }

    /// Real-font path: fill the cell background, then alpha-blend the
    /// rasterized (possibly multi-cell-wide, anti-aliased) glyph over it.
    ///
    /// `bg_alpha` is the window's configured background opacity (`255` =
    /// opaque). Any pixel touched by glyph coverage — including partially
    /// anti-aliased edge pixels — is always packed fully opaque, so text
    /// stays crisp and solid even in a translucent window; only untouched
    /// background pixels carry `bg_alpha`.
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
        bg_alpha: u8,
        fill_bg: bool,
    ) {
        let buf_width = canvas.width() as u32;
        let fg_px = if bg_alpha == 255 {
            fg.to_xrgb8888()
        } else {
            fg.premultiplied_argb8888(255)
        };
        let bg_px = Self::pack_bg(bg, bg_alpha);
        let px0 = u32::from(cx) * cell_w as u32;
        let py0 = u32::from(cy) * cell_h as u32;
        let cell_cols: u32 = if wide { 2 } else { 1 };

        {
            let pixels = canvas.pixels_mut();
            // Precompute the clipped scanline width once (rather than a
            // per-pixel bounds check + index multiply) and fill each row
            // with a single slice `fill` -- the background is almost always
            // a solid rectangle, so this is the hot path for every cell
            // drawn.
            let cell_px_w = cell_w as u32 * cell_cols;
            let clipped_w = if px0 >= buf_width {
                0
            } else {
                cell_px_w.min(buf_width - px0)
            };
            if fill_bg && clipped_w > 0 {
                for row in 0..cell_h as u32 {
                    let py = py0 + row;
                    let row_start = (py * buf_width + px0) as usize;
                    let row_end = row_start + clipped_w as usize;
                    if let Some(row_slice) = pixels.get_mut(row_start..row_end) {
                        row_slice.fill(bg_px);
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

        // `entry.bearing_y` is already the fully-resolved, single-rounded
        // offset from the cell's top row to the glyph bitmap's top row (see
        // `FontFace::cell_bearing_y`) -- it is *not* a baseline-relative
        // value, so it must not be combined with a separately-rounded
        // ascender here. Do not reintroduce `cache.ascender.round() -
        // bearing_y`; that was the double-rounding bug that made every
        // glyph's baseline jitter independently by up to a pixel.
        let glyph_x0 = px0 as i64 + i64::from(entry.bearing_x);
        let glyph_y0 = py0 as i64 + i64::from(entry.bearing_y);

        let pixels = canvas.pixels_mut();
        let atlas_w = cache.atlas.width;
        let buf_h = (pixels.len() / buf_width.max(1) as usize) as i64;

        // Clip the glyph's column span to the buffer ONCE, not per pixel. The
        // old inner loop paid a `px < 0 || px >= buf_width` branch on every
        // glyph pixel (perf: `blit_cell` was 60% of the real Face-path flood);
        // since `glyph_x0` is row-independent, the valid column range
        // `[col_lo, col_hi)` is a per-glyph constant. Same for rows via
        // `glyph_y0`. Empty span => nothing on screen.
        let col_lo = (-glyph_x0).max(0) as u32;
        let col_hi = (i64::from(buf_width) - glyph_x0)
            .clamp(0, i64::from(entry.width)) as u32;
        let row_lo = (-glyph_y0).max(0) as u32;
        let row_hi = (buf_h - glyph_y0).clamp(0, i64::from(entry.height)) as u32;
        if col_lo >= col_hi {
            return;
        }

        for row in row_lo..row_hi {
            let py = (glyph_y0 + i64::from(row)) as usize;
            // Hoist the row base indices out of the column loop: both the
            // destination row base (`py * buf_width`) and the atlas source
            // row base (`(entry.y + row) * atlas_w`) are per-row constants,
            // not per-pixel.
            let dst_row_base = py * buf_width as usize;
            let atlas_row_base = ((entry.y + row) * atlas_w) as usize;
            // Both `px` (via col_lo/col_hi) and `py` (via row_lo/row_hi) are
            // now provably in-bounds, so the inner loop is a checkless write.
            let dst_base = dst_row_base + (glyph_x0 + i64::from(col_lo)) as usize;
            let atlas_base = atlas_row_base + (entry.x + col_lo) as usize;
            for i in 0..(col_hi - col_lo) as usize {
                let a = cache.atlas.data[atlas_base + i];
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

    /// Blit every dirty row in `snap` into the canvas.
    ///
    /// When `snap.scroll_span == Some((top, bot, delta))` the content in rows
    /// `[top, bot]` moved up by `delta`. Rather than re-rasterize every cell,
    /// `copy_within` the backbuffer's scanlines up by `delta * cell_h`, then
    /// paint the snapshot's dirty rows on top -- which the grid has already
    /// placed at their true post-scroll positions, so an in-place edit
    /// batched in the same snapshot lands correctly instead of being clobbered
    /// by the shift. This is the classic terminal pixel-scroll optimization,
    /// but decoupled from the dirty-row set. Skipped for viewport
    /// (scrollback-scroll) snapshots, whose rows are an unrelated slice of
    /// history, not a continuation of the prior frame.
    ///
    /// Returns `Some((top, bot, delta))` when a pixel-scroll `copy_within` was
    /// performed: the backbuffer's scanlines for rows `top..=bot` were shifted
    /// up by `delta` rows. The caller needs this because any overlay it baked
    /// into the backbuffer last frame (cursor, selection tint, fps readout) got
    /// dragged up by the same shift, so its bookkeeping of "rows to repaint
    /// clean" must be shifted to match — otherwise the old overlay pixels are
    /// scrubbed at their pre-scroll row and the shifted copy is left behind as
    /// a static imprint.
    pub fn blit_snapshot(
        &mut self,
        canvas: &mut Canvas<'_>,
        snap: &DirtySnapshot,
    ) -> Option<ScrollBand> {
        // Rows of the glyph grid that fit in this buffer. A stale snapshot from a
        // taller geometry may carry rows past the bottom; clip them rather than let
        // them paint into the wrong pixels.
        let buf_width = canvas.width() as u32;
        let buf_rows = (canvas.pixels_mut().len() as u32 / buf_width.max(1)) / self.cell_h as u32;

        // Explicit scroll band: the grid tells us exactly which rows moved up
        // and by how much, decoupled from which rows are dirty. Pixel-shift the
        // band, then fall through to paint the snapshot's precise dirty rows on
        // top -- those now sit at their true post-scroll positions, so an
        // in-place edit batched alongside a scroll is painted correctly instead
        // of being clobbered by a whole-region re-blit or a bad shift.
        let mut scrolled: Option<ScrollBand> = None;
        if snap.viewport_offset().is_none() {
            if let Some(band) = snap.scroll_span() {
                let top = u32::from(band.top);
                let bot = u32::from(band.bot);
                let delta = u32::from(band.delta);
                if delta > 0 && delta <= bot - top && bot < buf_rows {
                    let row_scanlines = self.cell_h as usize;
                    let row_words = buf_width as usize * row_scanlines;
                    let keep_rows = (bot - top + 1 - delta) as usize;
                    let src_row = top as usize + delta as usize;
                    let dst_off = top as usize * row_words;
                    let src_off = src_row * row_words;
                    let pixels = canvas.pixels_mut();
                    pixels.copy_within(src_off..src_off + keep_rows * row_words, dst_off);
                    scrolled = Some(band);
                }
            }
        }

        // Paint every dirty row (they are already at correct post-scroll
        // positions). This covers both the genuinely-new bottom rows of a
        // scroll and any in-place edits batched in the same snapshot.
        for row in &snap.rows {
            if u32::from(row.row) >= buf_rows {
                continue;
            }
            self.blit_row(canvas, row.row, &row.cells);
        }
        scrolled
    }

    /// Overlay the cursor onto the canvas at a **fractional** cell position
    /// (`visual_col`/`visual_row`, in cell units -- e.g. `4.5` is halfway
    /// between columns 4 and 5), so a caller animating a slide between cells
    /// can render every intermediate frame, not just whole-cell positions.
    ///
    /// `alpha` (`0.0..=1.0`) is the current blink opacity; `0.0` paints
    /// nothing. The cursor alpha-blends a tint over whatever is already
    /// there rather than overwriting it, so the glyph underneath stays
    /// legible through a translucent or mid-fade cursor (matching
    /// Ghostty/WezTerm's default look).
    pub fn draw_cursor(
        &self,
        canvas: &mut Canvas<'_>,
        visual_col: f32,
        visual_row: f32,
        shape: grid::CursorShape,
        alpha: f32,
    ) {
        if alpha <= 0.0 {
            return;
        }
        let alpha = alpha.min(1.0);
        let (fg, _) = self.theme.effective(&Attrs::default());
        let buf_w = canvas.width();
        let buf_h = canvas.height();
        let px0 = visual_col * self.cell_w as f32;
        let py0 = visual_row * self.cell_h as f32;

        let (rx0, ry0, rx1, ry1) = match shape {
            grid::CursorShape::Block => {
                (px0, py0, px0 + self.cell_w as f32, py0 + self.cell_h as f32)
            }
            grid::CursorShape::Bar => {
                // Slightly narrower than before, and inset a hair
                // vertically so it doesn't touch neighbouring lines --
                // reads as a slim text-caret rather than a full-height bar.
                const BAR_W: f32 = 1.5;
                const VERT_INSET: f32 = 1.5;
                (
                    px0,
                    py0 + VERT_INSET,
                    px0 + BAR_W,
                    py0 + self.cell_h as f32 - VERT_INSET,
                )
            }
            grid::CursorShape::Underline => {
                const UNDERLINE_H: f32 = 2.0;
                (
                    px0,
                    py0 + self.cell_h as f32 - UNDERLINE_H,
                    px0 + self.cell_w as f32,
                    py0 + self.cell_h as f32,
                )
            }
        };

        let x0 = rx0.floor().max(0.0) as i32;
        let y0 = ry0.floor().max(0.0) as i32;
        let x1 = rx1.ceil().min(buf_w as f32) as i32;
        let y1 = ry1.ceil().min(buf_h as f32) as i32;
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let pixels = canvas.pixels_mut();
        let fg_r = f32::from(fg.r);
        let fg_g = f32::from(fg.g);
        let fg_b = f32::from(fg.b);
        for y in y0..y1 {
            for x in x0..x1 {
                let idx = (y as usize) * (buf_w as usize) + (x as usize);
                let Some(dst) = pixels.get_mut(idx) else {
                    continue;
                };
                let d = *dst;
                let dr = f32::from(((d >> 16) & 0xff) as u8);
                let dg = f32::from(((d >> 8) & 0xff) as u8);
                let db = f32::from((d & 0xff) as u8);
                let nr = (fg_r - dr).mul_add(alpha, dr).round() as u32;
                let ng = (fg_g - dg).mul_add(alpha, dg).round() as u32;
                let nb = (fg_b - db).mul_add(alpha, db).round() as u32;
                *dst = (d & 0xff00_0000) | (nr << 16) | (ng << 8) | nb;
            }
        }
    }

    /// Draw the "landing pulse" ring: a thin outline centered on the cell
    /// at `(visual_col, visual_row)`, expanded outward and faded out as `t`
    /// (`0.0` = just landed, `1.0` = fully gone) advances. Used after a long
    /// cursor jump (e.g. a multi-line paste) settles, to draw the eye to
    /// where the cursor ended up.
    pub fn draw_cursor_pulse(
        &self,
        canvas: &mut Canvas<'_>,
        visual_col: f32,
        visual_row: f32,
        t: f32,
    ) {
        if !(0.0..1.0).contains(&t) {
            return;
        }
        let (fg, _) = self.theme.effective(&Attrs::default());
        let buf_w = canvas.width();
        let buf_h = canvas.height();

        // Ring grows outward from the cell bounds by up to ~60% of a cell,
        // and fades out over the same span -- an expanding, dissolving box.
        let grow = t * (self.cell_w.min(self.cell_h) as f32) * 0.6;
        let alpha = 1.0 - t;
        let x0f = visual_col.mul_add(self.cell_w as f32, -grow);
        let y0f = visual_row.mul_add(self.cell_h as f32, -grow);
        let x1f = x0f + self.cell_w as f32 + grow * 2.0;
        let y1f = y0f + self.cell_h as f32 + grow * 2.0;

        // Clamp to the grid-aligned content area, not just the raw buffer
        // size. A window's pixel size is rarely an exact multiple of the
        // cell size, leaving a thin dead strip along the bottom/right edge
        // that `repaint_rows` never touches (it only repaints whole grid
        // rows from the cell mirror). Letting the ring bleed into that strip
        // stamps pixels there that nothing ever erases again -- stray white
        // boxes stuck at the edge of the window forever. Clip to the last
        // full cell's bounds instead so the ring never leaves the area the
        // normal repaint path actually owns.
        let grid_max_x = (buf_w / self.cell_w) * self.cell_w;
        let grid_max_y = (buf_h / self.cell_h) * self.cell_h;

        let x0 = x0f.floor().max(0.0) as i32;
        let y0 = y0f.floor().max(0.0) as i32;
        let x1 = x1f.ceil().min(buf_w as f32).min(grid_max_x as f32) as i32;
        let y1 = y1f.ceil().min(buf_h as f32).min(grid_max_y as f32) as i32;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        const THICK: i32 = 2;
        let pixels = canvas.pixels_mut();
        let fg_r = f32::from(fg.r);
        let fg_g = f32::from(fg.g);
        let fg_b = f32::from(fg.b);
        let mut blend_px = |x: i32, y: i32| {
            if x < 0 || y < 0 || x >= buf_w || y >= buf_h {
                return;
            }
            let idx = (y as usize) * (buf_w as usize) + (x as usize);
            let Some(dst) = pixels.get_mut(idx) else {
                return;
            };
            let d = *dst;
            let dr = f32::from(((d >> 16) & 0xff) as u8);
            let dg = f32::from(((d >> 8) & 0xff) as u8);
            let db = f32::from((d & 0xff) as u8);
            let nr = (fg_r - dr).mul_add(alpha, dr).round() as u32;
            let ng = (fg_g - dg).mul_add(alpha, dg).round() as u32;
            let nb = (fg_b - db).mul_add(alpha, db).round() as u32;
            *dst = (d & 0xff00_0000) | (nr << 16) | (ng << 8) | nb;
        };
        for y in y0..y1 {
            let on_h_edge = y < y0 + THICK || y >= y1 - THICK;
            if on_h_edge {
                for x in x0..x1 {
                    blend_px(x, y);
                }
            } else {
                for x in x0..(x0 + THICK).min(x1) {
                    blend_px(x, y);
                }
                for x in (x1 - THICK).max(x0)..x1 {
                    blend_px(x, y);
                }
            }
        }
    }
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
    fn batched_row_fill_is_byte_identical_to_per_cell_fill() {
        // The batched `blit_row` path (one wide fill per background run, glyphs
        // stamped on top) must produce exactly the same pixels the old
        // per-cell path did (each cell filled its own bg, then its glyph). We
        // prove it by painting the same row two ways into two buffers and
        // asserting byte equality -- across a mix of glyphs, spaces, and
        // differing backgrounds (so multiple runs exist), plus a wide glyph
        // with its trailing spacer (which must inherit the lead's background).
        let mut red = Attrs::default();
        red.bg = grid::Color::Indexed(1);
        let mut blue = Attrs::default();
        blue.bg = grid::Color::Indexed(4);

        let attr_cell = |ch: char, a: &Attrs, wide: bool, spacer: bool| Cell {
            ch,
            attrs: *a,
            wide,
            spacer,
        };
        let def = Attrs::default();
        let cells = vec![
            attr_cell('H', &def, false, false),
            attr_cell('i', &def, false, false),
            attr_cell(' ', &red, false, false), // bg run change
            attr_cell('X', &red, false, false),
            attr_cell(' ', &def, false, false), // back to default bg
            attr_cell('W', &blue, true, false), // wide lead
            attr_cell(' ', &blue, false, true), // spacer inherits blue bg
            attr_cell('!', &def, false, false),
        ];

        let cols = cells.len() as u32;
        let bw = cols * GLYPH_W as u32;
        let bh = GLYPH_H as u32;

        // Reference: force the old per-cell path (fill_bg = true for every
        // cell, no batching, no blank-skip).
        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut canvas = Canvas::new(&mut reference, bw as i32, bh as i32);
            for (col, c) in cells.iter().enumerate() {
                r.blit_cell_inner(&mut canvas, col as u16, 0, c, true);
            }
        }

        // Batched path under test.
        let mut batched = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut canvas = Canvas::new(&mut batched, bw as i32, bh as i32);
            r.blit_row(&mut canvas, 0, &cells);
        }

        assert_eq!(batched, reference, "batched blit_row diverged from per-cell");
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
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: grid::SnapshotKind::Incremental { scroll_span: None },
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
    fn content_scroll_pixel_shifts_backbuffer_instead_of_reblitting_unchanged_rows() {
        // A 1x4 grid (arbitrary width, 4 rows). Paint distinct glyphs into
        // each row first via a normal (non-scrolled) snapshot, then send a
        // content_delta=1 snapshot whose dirty rows are the classic
        // "whole-region redirtied by a scroll" shape: rows 0..=3 all
        // reported dirty, where rows 0..=2 are just the old rows 1..=3
        // shifted up (unchanged content) and row 3 is genuinely new.
        let bw = GLYPH_W as u32;
        let bh = 4 * GLYPH_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];
        let mut renderer = Renderer::builtin(Theme::builtin());

        let initial = DirtySnapshot {
            cols: 1,
            rows: vec![
                DirtyRow {
                    row: 0,
                    cells: vec![cell('A')],
                },
                DirtyRow {
                    row: 1,
                    cells: vec![cell('B')],
                },
                DirtyRow {
                    row: 2,
                    cells: vec![cell('C')],
                },
                DirtyRow {
                    row: 3,
                    cells: vec![cell('D')],
                },
            ],
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: grid::SnapshotKind::Incremental { scroll_span: None },
        };
        {
            let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
            renderer.blit_snapshot(&mut canvas, &initial);
        }
        // Snapshot the pixels for what row 1 (glyph 'B') looked like -- after
        // a scroll-up by 1, this should reappear verbatim at row 0.
        let row_len = (bw * GLYPH_H as u32) as usize;
        let old_row1: Vec<u32> = pixels[row_len..2 * row_len].to_vec();
        let old_row2: Vec<u32> = pixels[2 * row_len..3 * row_len].to_vec();
        let old_row3: Vec<u32> = pixels[3 * row_len..4 * row_len].to_vec();

        // Now the scroll: content moved up by 1 line, so old rows 1,2,3
        // are now at 0,1,2, and row 3 is fresh content ('E').
        let scrolled = DirtySnapshot {
            cols: 1,
            rows: vec![
                DirtyRow {
                    row: 0,
                    cells: vec![cell('B')],
                },
                DirtyRow {
                    row: 1,
                    cells: vec![cell('C')],
                },
                DirtyRow {
                    row: 2,
                    cells: vec![cell('D')],
                },
                DirtyRow {
                    row: 3,
                    cells: vec![cell('E')],
                },
            ],
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 1,
            kind: grid::SnapshotKind::Incremental {
                scroll_span: Some(ScrollBand {
                    top: 0,
                    bot: 3,
                    delta: 1,
                }),
            },
        };
        let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
        renderer.blit_snapshot(&mut canvas, &scrolled);

        // Row 0 must now be byte-identical to the old row 1 ('B') -- proof
        // it was copied via copy_within, not re-rasterized (re-rasterizing
        // 'B' would produce the same glyph too, so this alone isn't
        // conclusive, but combined with the other rows it demonstrates the
        // shift happened contiguously).
        assert_eq!(&pixels[0..row_len], &old_row1[..]);
        assert_eq!(&pixels[row_len..2 * row_len], &old_row2[..]);
        assert_eq!(&pixels[2 * row_len..3 * row_len], &old_row3[..]);
        // Row 3 is fresh -- must differ from the old row 3 ('D''s pixels),
        // since it now holds 'E'.
        assert_ne!(&pixels[3 * row_len..4 * row_len], &old_row3[..]);
    }

    // Build a full-screen scroll snapshot: the grid scrolled up by `delta`,
    // so visible row i now shows logical line `base_line + i`. Every visible
    // row is dirty, content_delta == delta, and scroll_span covers the whole
    // screen (rows 0..=rows-1) shifted up by `delta` when `delta > 0`.
    fn scroll_snap(cols: u16, rows: u16, base_line: u32, delta: u16) -> DirtySnapshot {
        let dirty = (0..rows)
            .map(|r| {
                let line = base_line + u32::from(r);
                // Distinct glyph per logical line so overlap/misalignment shows.
                let ch = char::from(b'a' + (line % 26) as u8);
                DirtyRow {
                    row: r,
                    cells: (0..cols).map(|_| cell(ch)).collect(),
                }
            })
            .collect();
        DirtySnapshot {
            cols,
            rows: dirty,
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: delta,
            kind: grid::SnapshotKind::Incremental {
                scroll_span: if delta > 0 {
                    Some(ScrollBand {
                        top: 0,
                        bot: rows - 1,
                        delta,
                    })
                } else {
                    None
                },
            },
        }
    }

    #[test]
    fn fast_pixel_scroll_matches_full_reblit_across_composed_frames() {
        // The heart of the flood-jank question: after a sequence of scroll
        // snapshots (some composed into one present), the fast pixel-scroll
        // path must leave the backbuffer byte-identical to a from-scratch full
        // re-blit of the final visible content. If it diverges, the live
        // display garbles even though the grid (scrollback) is correct --
        // exactly the reported symptom.
        let cols = 4u16;
        let rows = 8u16;
        let bw = cols as u32 * GLYPH_W as u32;
        let bh = rows as u32 * GLYPH_H as u32;

        // Reference renderer: full re-blit of the final state (delta forced 0
        // so it never takes the fast path).
        let final_base = 100u32;
        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut full = scroll_snap(cols, rows, final_base, 0);
            full.content_delta = 0;
            let mut canvas = Canvas::new(&mut reference, bw as i32, bh as i32);
            r.blit_snapshot(&mut canvas, &full);
        }

        // Fast-path renderer: reach the SAME final state by scrolling there in
        // steps. Seed with the pre-scroll frame (full blit), then apply scroll
        // snapshots whose deltas sum so the final visible top line == final_base.
        let mut fast = vec![0u32; (bw * bh) as usize];
        let mut r = Renderer::builtin(Theme::builtin());
        // Seed: visible top line = final_base - total_scroll.
        let steps = [1u16, 2, 3]; // composed deltas; sum = 6
        let total: u32 = steps.iter().map(|&d| u32::from(d)).sum();
        let seed_base = final_base - total;
        {
            let mut seed = scroll_snap(cols, rows, seed_base, 0);
            seed.content_delta = 0;
            let mut canvas = Canvas::new(&mut fast, bw as i32, bh as i32);
            r.blit_snapshot(&mut canvas, &seed);
        }
        // Now scroll to final in steps, each a fast-path snapshot.
        let mut acc = 0u32;
        {
            let mut canvas = Canvas::new(&mut fast, bw as i32, bh as i32);
            for &d in &steps {
                acc += u32::from(d);
                let base = seed_base + acc;
                let snap = scroll_snap(cols, rows, base, d);
                let scrolled = r.blit_snapshot(&mut canvas, &snap);
                assert!(
                    scrolled.is_some(),
                    "delta={d} should take the fast pixel-scroll path"
                );
            }
        }

        assert_eq!(
            fast, reference,
            "fast pixel-scroll diverged from full re-blit of the same final content"
        );
    }

    #[test]
    fn pixel_scroll_reports_shifted_band_for_overlay_cleanup() {
        // The pixel-scroll path must tell the caller which rows it shifted and
        // by how much, so baked overlays (cursor/selection/fps) can have their
        // repaint bookkeeping remapped -- otherwise a dragged-up cursor imprint
        // is left behind. A non-scroll snapshot must report None.
        let bw = GLYPH_W as u32;
        let bh = 4 * GLYPH_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];
        let mut renderer = Renderer::builtin(Theme::builtin());

        let full = DirtySnapshot {
            cols: 1,
            rows: vec![
                DirtyRow {
                    row: 0,
                    cells: vec![cell('A')],
                },
                DirtyRow {
                    row: 1,
                    cells: vec![cell('B')],
                },
                DirtyRow {
                    row: 2,
                    cells: vec![cell('C')],
                },
                DirtyRow {
                    row: 3,
                    cells: vec![cell('D')],
                },
            ],
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: grid::SnapshotKind::Incremental { scroll_span: None },
        };
        {
            let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
            // A plain repaint is not a scroll -- no shift to report.
            assert_eq!(renderer.blit_snapshot(&mut canvas, &full), None);
        }

        let scrolled = DirtySnapshot {
            cols: 1,
            rows: vec![
                DirtyRow {
                    row: 0,
                    cells: vec![cell('B')],
                },
                DirtyRow {
                    row: 1,
                    cells: vec![cell('C')],
                },
                DirtyRow {
                    row: 2,
                    cells: vec![cell('D')],
                },
                DirtyRow {
                    row: 3,
                    cells: vec![cell('E')],
                },
            ],
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 1,
            kind: grid::SnapshotKind::Incremental {
                scroll_span: Some(ScrollBand {
                    top: 0,
                    bot: 3,
                    delta: 1,
                }),
            },
        };
        let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
        // Rows 0..=3 shifted up by 1.
        assert_eq!(
            renderer.blit_snapshot(&mut canvas, &scrolled),
            Some(ScrollBand {
                top: 0,
                bot: 3,
                delta: 1
            })
        );
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
            cursor: grid::CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: grid::SnapshotKind::Incremental { scroll_span: None },
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
    fn builtin_scales_cell_size_by_output_scale() {
        // The bitmap fallback path must also scale, not just the TTF path --
        // a HiDPI output with no configured font still shouldn't render a
        // blurry 1x bitmap upscaled by the compositor.
        let r = Renderer::with_scale(Theme::builtin(), &config::FontConfig::default(), 2);
        assert_eq!(r.cell_w(), GLYPH_W * 2);
        assert_eq!(r.cell_h(), GLYPH_H * 2);
        assert_eq!(r.output_scale(), 2);
    }

    #[test]
    fn with_scale_one_matches_new() {
        let a = Renderer::new(Theme::builtin(), &config::FontConfig::default());
        let b = Renderer::with_scale(Theme::builtin(), &config::FontConfig::default(), 1);
        assert_eq!(a.cell_w(), b.cell_w());
        assert_eq!(a.cell_h(), b.cell_h());
        assert_eq!(a.output_scale(), 1);
    }

    #[test]
    fn with_scale_clamps_zero_and_negative_to_one() {
        let r = Renderer::with_scale(Theme::builtin(), &config::FontConfig::default(), 0);
        assert_eq!(r.output_scale(), 1);
        let r = Renderer::with_scale(Theme::builtin(), &config::FontConfig::default(), -3);
        assert_eq!(r.output_scale(), 1);
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
        // bg_alpha = 255 (opaque window): the pre-M10 XRGB8888 behavior.
        assert_eq!(fg.blend_glyph(bg, 0, 255), bg.to_xrgb8888());
        assert_eq!(fg.blend_glyph(bg, 255, 255), fg.to_xrgb8888());
    }

    #[test]
    fn blend_translucent_window_zero_glyph_coverage_is_translucent_bg() {
        let bg = DrawColor::rgb(10, 20, 30);
        let fg = DrawColor::rgb(255, 255, 255);
        // No glyph ink at all: falls back to the plain translucent background
        // pixel, alpha == bg_alpha exactly. wl_shm wants *premultiplied*
        // alpha, so the color channels are also scaled by bg_alpha/255 (128
        // -> ~0.502), not carried through straight.
        let px = fg.blend_glyph(bg, 0, 128);
        assert_eq!(px >> 24, 128);
        assert_eq!((px >> 16) & 0xff, 5, "10 * 128/255 rounds to 5");
        assert_eq!((px >> 8) & 0xff, 10, "20 * 128/255 rounds to 10");
        assert_eq!(px & 0xff, 15, "30 * 128/255 rounds to 15");
    }

    #[test]
    fn blend_translucent_window_full_glyph_coverage_is_opaque_fg() {
        let bg = DrawColor::rgb(10, 20, 30);
        let fg = DrawColor::rgb(200, 100, 50);
        // glyph_a == 255 short-circuits in blit_cell_face before calling
        // blend_glyph, but blend_glyph itself must still be correct if
        // called directly: full ink coverage means fully opaque foreground
        // regardless of the window's glass opacity.
        let px = fg.blend_glyph(bg, 255, 128);
        assert_eq!(px >> 24, 255, "full glyph coverage must be fully opaque");
        assert_eq!((px >> 16) & 0xff, 200);
        assert_eq!((px >> 8) & 0xff, 100);
        assert_eq!(px & 0xff, 50);
    }

    #[test]
    fn renderer_with_opacity_reports_needs_alpha() {
        let r = Renderer::builtin(Theme::builtin());
        assert!(!r.needs_alpha());
        let r = r.with_opacity(0.8);
        assert!(r.needs_alpha());
        let r = r.with_opacity(1.0);
        assert!(!r.needs_alpha());
    }

    #[test]
    fn renderer_bg_packed_matches_bg_xrgb_when_opaque() {
        let r = Renderer::builtin(Theme::builtin());
        assert_eq!(r.bg_packed(), r.bg_xrgb());
    }

    #[test]
    fn cursor_pulse_does_not_paint_past_the_grid_aligned_area() {
        // A buffer size that is NOT an exact multiple of the cell size --
        // the common case after a mouse resize -- leaving a dead strip along
        // the bottom and right edges that `repaint_rows` never touches.
        let cell_w = GLYPH_W as u32;
        let cell_h = GLYPH_H as u32;
        let extra_x = 5; // sub-cell leftover strip on the right
        let extra_y = 5; // sub-cell leftover strip on the bottom
        let cols = 4u32;
        let rows = 4u32;
        let bw = cols * cell_w + extra_x;
        let bh = rows * cell_h + extra_y;
        let mut pixels = vec![0u32; (bw * bh) as usize];

        let renderer = Renderer::builtin(Theme::builtin());
        let mut canvas = Canvas::new(&mut pixels, bw as i32, bh as i32);
        // Land the cursor at the bottom-right cell, mid-pulse (t is picked so
        // `grow` is near its max -- the worst case for bleeding past the
        // grid-aligned area).
        renderer.draw_cursor_pulse(&mut canvas, (cols - 1) as f32, (rows - 1) as f32, 0.99);

        // Nothing in the dead strip (past the last full cell) may have been
        // touched -- those pixels are never repainted again by the normal
        // row-based repaint path, so any stray ink there is permanent.
        for y in 0..bh {
            for x in 0..bw {
                if x >= cols * cell_w || y >= rows * cell_h {
                    let px = pixels[(y * bw + x) as usize];
                    assert_eq!(px, 0, "dead-strip pixel ({x},{y}) was painted: {px:#x}");
                }
            }
        }
    }

    #[test]
    fn renderer_bg_packed_carries_alpha_when_translucent() {
        let r = Renderer::builtin(Theme::builtin()).with_opacity(0.5);
        let packed = r.bg_packed();
        let alpha = packed >> 24;
        // 0.5 * 255 rounds to 128 (127.5 -> 128).
        assert_eq!(alpha, 128);
    }
}
