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

/// xterm 256-color palette: indices 0–255 → (r, g, b). Re-exported from
/// `grid` (the single source of truth -- see `grid::palette` for why this
/// used to be a second copy living here alone, before OSC 4 needed the grid
/// thread to know the same table too).
use grid::XTERM_256 as PALETTE;

/// Built-in default fg/bg (used when no config theme is supplied).
const DEFAULT_FG: DrawColor = DrawColor::rgb(0xcc, 0xcc, 0xcc);
const DEFAULT_BG: DrawColor = DrawColor::rgb(0x1e, 0x1e, 0x2e);

/// Resolved color theme: the 16 ANSI colors plus default fg/bg.
///
/// Indices 16–255 fall through to the static xterm cube. Built from a
/// [`config::Palette`] via [`Theme::from_palette`], or [`Theme::builtin`] for
/// the hard-coded default.
#[derive(Clone)]
pub struct Theme {
    /// ANSI 0–15 overrides (RGB).
    ansi: [(u8, u8, u8); 16],
    fg: DrawColor,
    bg: DrawColor,
    /// OSC 4 live palette overrides for indices 16–255 (the cube/grayscale
    /// range not covered by `ansi`). `None` means "still the static
    /// `PALETTE` default". Boxed for the same reason `grid::Grid` boxes its
    /// copy of this: 256 `Option<(u8,u8,u8)>` slots is ~1KB, too big to want
    /// inline in a struct that's cloned on every config live-reload.
    /// Indices 0–15 route through `ansi` instead (already independently
    /// overridable via config, and merging the two would mean every
    /// `resolve()` call -- the actual per-cell hot path -- pays for an extra
    /// branch that's dead 100% of the time outside of an active OSC 4
    /// session).
    cube_overrides: Box<[Option<(u8, u8, u8)>; 256]>,
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
            cube_overrides: Box::new([None; 256]),
        }
    }

    /// Build a theme from a parsed config palette.
    #[must_use]
    pub fn from_palette(p: &config::Palette) -> Self {
        let mut ansi = [(0u8, 0u8, 0u8); 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            // `ansi` has 16 elements, so `i` always fits a `u8`.
            let c = p.ansi(u8::try_from(i).unwrap_or(0));
            *slot = (c.r, c.g, c.b);
        }
        Self {
            ansi,
            fg: DrawColor::rgb(p.foreground.r, p.foreground.g, p.foreground.b),
            bg: DrawColor::rgb(p.background.r, p.background.g, p.background.b),
            cube_overrides: Box::new([None; 256]),
        }
    }

    /// Live-patch palette index `n` (an OSC 4 write) without rebuilding the
    /// whole theme. `n < 16` updates `ansi` (config-overridable ANSI
    /// colors); `n >= 16` updates `cube_overrides`. The caller (the UI
    /// thread, draining `Grid::take_palette_changes`) is responsible for
    /// forcing a repaint afterwards -- this only changes what a *future*
    /// `resolve()` call returns, it doesn't touch anything already painted.
    pub fn set_palette_color(&mut self, n: u8, rgb: (u8, u8, u8)) {
        if n < 16 {
            self.ansi[n as usize] = rgb;
        } else {
            self.cube_overrides[n as usize] = Some(rgb);
        }
    }

    /// The theme background as a packed XRGB8888 word (backbuffer clear color).
    #[must_use]
    pub const fn bg_xrgb(&self) -> u32 {
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
                let (r, g, b) = self.cube_overrides[n as usize].unwrap_or(PALETTE[n as usize]);
                DrawColor::rgb(r, g, b)
            }
            Color::Rgb(r, g, b) => DrawColor::rgb(r, g, b),
        }
    }

    fn effective(&self, attrs: &Attrs) -> (DrawColor, DrawColor) {
        let mut fg = self.resolve(attrs.fg, self.fg);
        let bg = self.resolve(attrs.bg, self.bg);
        // Intensity attributes fold into the resolved foreground:
        //   * `dim`  (SGR 2) → lerp fg ~40% toward bg, the conventional
        //     "faint" look. Used by pagers, `ls` metadata, diff context, etc.
        //   * `bold` (SGR 1) with a *palette* color → promote 0–7 to the
        //     bright 8–15 slot (the classic xterm "bold is bright"), leaving
        //     truecolor/default fg untouched. Real weight selection would need
        //     a bold face in the atlas; brightening is the widely-expected
        //     fallback and is what makes bold text visibly pop.
        // `dim` wins if somehow both are set (SGR 1;2 is contradictory; most
        // terminals let the later-applied intensity stand, and grid clears
        // both on NormalIntensity, so this only bites pathological input).
        if attrs.bold()
            && !attrs.dim()
            && let Color::Indexed(n) = attrs.fg
            && n < 8
        {
            fg = self.resolve(Color::Indexed(n + 8), self.fg);
        }
        if attrs.dim() {
            // 60% of the way from fg toward bg: `blend_opaque_to` lerps
            // `self → to` by `alpha/255`, so start from bg and pull toward fg
            // by ~40% to land a faint-but-legible grey.
            let faint = bg.blend_opaque_to(fg, 102);
            // Extracting one byte from a packed-u32 color: an intentional
            // truncation (mask via `& 0xFF`), not a lossy numeric coercion.
            fg = DrawColor::rgb(
                u8::try_from((faint >> 16) & 0xFF).unwrap_or(0),
                u8::try_from((faint >> 8) & 0xFF).unwrap_or(0),
                u8::try_from(faint & 0xFF).unwrap_or(0),
            );
        }
        if attrs.inverse() { (bg, fg) } else { (fg, bg) }
    }
}

// ── Glyph sources ─────────────────────────────────────────────────────────────

/// A real TTF/OTF face plus its glyph atlas, growing on demand as new
/// codepoints are drawn. Pre-seeded with printable ASCII + box-drawing so the
/// common case never rasterizes at paint time.
///
/// Optionally chained to a `fallback` face (see [`Self::with_fallback`]):
/// codepoints the primary `face` doesn't cover (most commonly emoji --
/// monospace programming fonts essentially never ship them) are rasterized
/// from `fallback` instead of coming out as the primary face's `.notdef`
/// "tofu" box. Both faces share one atlas/map keyed by codepoint, and every
/// glyph -- regardless of which face produced it -- is positioned against
/// this cache's *own* `ascender` (the primary face's line metrics), so a
/// fallback glyph still sits on the same baseline as its neighbors instead
/// of using the fallback font's unrelated line metrics.
struct GlyphCache {
    face: font::FontFace,
    atlas: font::Atlas,
    map: HashMap<u32, font::GlyphEntry>,
    /// Distance from the top of the cell to the baseline, in pixels.
    ascender: f32,
    /// Fallback face consulted when `face` lacks a glyph for some codepoint
    /// -- see the struct doc. `None` when no fallback font is configured
    /// (or the fallback failed to load), in which case an uncovered
    /// codepoint rasterizes `face`'s own `.notdef` glyph, same as before
    /// fallback support existed.
    fallback: Option<font::FontFace>,
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
            fallback: None,
        }
    }

    /// Attach a fallback face (e.g. an emoji font) consulted for codepoints
    /// the primary face doesn't cover. `None` leaves the cache unchanged
    /// (no fallback configured / load failed upstream).
    fn with_fallback(mut self, fallback: Option<font::FontFace>) -> Self {
        self.fallback = fallback;
        self
    }

    /// Look up (rasterizing + atlas-inserting on first use) the glyph entry
    /// for `codepoint`. `None` if the atlas is full (extremely large glyph
    /// sets) — the caller then just paints background for that cell.
    // Glyph bearing/advance are always tiny (well under a few hundred
    // pixels), so `.round() as i32` never truncates meaningfully; stable
    // Rust has no checked float->int API to express that more precisely
    // than `as` (see `font::raster`'s cast-policy note for the general
    // rationale this crate follows).
    #[allow(clippy::cast_possible_truncation)]
    fn entry(&mut self, codepoint: u32) -> Option<font::GlyphEntry> {
        if let Some(e) = self.map.get(&codepoint) {
            return Some(*e);
        }
        // Route to the fallback face only when the primary face genuinely
        // lacks the glyph -- `has_glyph` is a cheap cmap-only check, so this
        // costs nothing on the overwhelmingly common case (primary face has
        // it) and only consults the emoji font for codepoints it can't.
        //
        // `self.face`/`self.fallback` and `self.atlas` are disjoint fields,
        // so borrowing one of the two former (whichever branch runs) and
        // then `self.atlas` below doesn't fight the borrow checker despite
        // the mutable + immutable overlap.
        let use_fallback = !self.face.has_glyph(codepoint) && self.fallback.is_some();
        let g = if use_fallback {
            self.fallback
                .as_mut()
                .expect("use_fallback implies fallback.is_some()")
                .rasterize(codepoint)
        } else {
            self.face.rasterize(codepoint)
        };
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

/// Owns the color theme and glyph source, and knows how to blit grid cells into a [`Canvas`].
///
/// Replaces the old free-standing `Canvas::blit_cell` / `blit_snapshot`
/// methods so the (now dynamic, font-dependent) cell size and the glyph
/// cache have somewhere to live between frames.
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
    /// (1 = standard DPI, 2 = `HiDPI` @2x, ...). The font was rasterized at
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
    /// The scrollback viewport offset the backbuffer currently shows, as far
    /// as the *renderer* knows -- `None` until the first viewport frame lands.
    /// This is the ground truth for computing a scroll-back pixel-shift: it
    /// advances ONLY when a viewport frame is actually blitted here, so a
    /// snapshot the parser thread built but never delivered (requeued on a
    /// full ring) can't desync it. The parser's own optimistic guess
    /// (`Grid::rendered_offset`) tears under load precisely because it assumes
    /// 1:1 delivery; owning the offset on the side that does the blit removes
    /// that whole class of bug.
    last_viewport_offset: Option<usize>,
    /// Per-row scratch cache of `theme.effective(cell.attrs)` results, keyed
    /// by column, reused across every `blit_row` call to avoid a Vec
    /// allocation per row. `blit_row`'s background pass (pass 1) computes
    /// each visible cell's `(fg, bg)` exactly once and stores it here; the
    /// glyph pass (pass 2) reads it back instead of recomputing -- resolving
    /// a cell's effective colors means a palette/indexed lookup plus, for
    /// bold/dim, an extra branch and lerp, so doing it twice per cell (as
    /// the pre-cache code did: once in each pass) was pure waste on the
    /// `blit` hot path. Grows to fit the widest row seen; never shrinks.
    row_color_cache: Vec<(DrawColor, DrawColor)>,
    /// Observability for the scrollback pixel-shift path: how many rows the
    /// last `blit_snapshot` actually rasterized (vs. moved by `copy_within`),
    /// and the signed shift it applied (`0` = full repaint / not a viewport
    /// frame). The whole point of the optimization is that a wheel step
    /// rasterizes only the freshly-revealed edge rows, so these let a
    /// regression test assert the shift is engaging instead of silently
    /// falling back to a full-screen repaint (the fps cliff). Behind `trace`
    /// so release builds carry zero extra per-frame state.
    #[cfg(feature = "trace")]
    pub last_rasterized_rows: usize,
    #[cfg(feature = "trace")]
    pub last_viewport_shift: i32,
}

impl Renderer {
    /// Build a renderer using only the embedded bitmap font (no TTF/OTF
    /// loading). Used by tests and as the fallback path of [`Self::new`].
    #[must_use]
    pub const fn builtin(theme: Theme) -> Self {
        Self {
            theme,
            glyphs: GlyphSource::Bitmap,
            cell_w: GLYPH_W,
            cell_h: GLYPH_H,
            output_scale: 1,
            bg_alpha: 255,
            last_viewport_offset: None,
            row_color_cache: Vec::new(),
            #[cfg(feature = "trace")]
            last_rasterized_rows: 0,
            #[cfg(feature = "trace")]
            last_viewport_shift: 0,
        }
    }

    /// Set the window background opacity, `0.0` (fully transparent) to `1.0`
    /// (opaque). Only affects pure-background pixels — glyph pixels are
    /// always packed fully opaque (see [`Self::bg_alpha`]). Values outside
    /// `[0.0, 1.0]` are clamped; `config::WindowConfig::opacity` is already
    /// validated to that range, so this is just defense in depth.
    // `opacity.clamp(0.0, 1.0) * 255.0` is always in `0.0..=255.0`, so the
    // rounding cast never truncates/loses sign meaningfully; stable Rust has
    // no checked float->int API (see `font::raster`'s cast-policy note).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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

    /// Live-patch one palette index (an OSC 4 write from the grid thread)
    /// without rebuilding the renderer. The caller still owes a full repaint
    /// after this -- see [`crate::window::TermWindow::apply_palette_change`],
    /// which is the only real caller and does exactly that.
    pub fn set_palette_color(&mut self, n: u8, rgb: (u8, u8, u8)) {
        self.theme.set_palette_color(n, rgb);
    }

    /// Build a renderer from config: tries to load `font_cfg.path` at
    /// `font_cfg.size` pixels; falls back to the embedded bitmap font (with a
    /// diagnostic on stderr) if no path is configured or loading fails.
    ///
    /// Equivalent to `Self::with_scale(theme, font_cfg, 1)` — kept as the
    /// zero-argument entry point for callers (tests, anything not talking to
    /// a real scaled Wayland output) that don't care about `HiDPI`.
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
    // output_scale is a small integer scale factor (1, 2, 3...) -- always
    // exactly representable as f32, no precision loss possible. The
    // `cell_w`/`cell_h` rounding casts below are always in a tiny pixel
    // range with no checked float->int API in stable Rust to express that
    // more precisely (see `font::raster`'s cast-policy note).
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
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
                let fallback = Self::load_emoji_fallback(font_cfg, px_size);
                Self {
                    theme,
                    glyphs: GlyphSource::Face(Box::new(
                        GlyphCache::new(face).with_fallback(fallback),
                    )),
                    cell_w,
                    cell_h,
                    output_scale,
                    bg_alpha: 255,
                    last_viewport_offset: None,
                    row_color_cache: Vec::new(),
                    #[cfg(feature = "trace")]
                    last_rasterized_rows: 0,
                    #[cfg(feature = "trace")]
                    last_viewport_shift: 0,
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

    /// Load the emoji fallback face for [`Self::with_scale`]'s `GlyphCache`
    /// from `font_cfg.emoji_path`. There is no bundled/embedded fallback --
    /// if the user hasn't configured `[font] emoji_path`, this returns
    /// `None` and emoji-range codepoints the primary face lacks just render
    /// as `.notdef` (same as before fallback support existed). If a path
    /// IS configured but fails to load, a diagnostic is printed and this
    /// also returns `None` rather than aborting startup.
    fn load_emoji_fallback(font_cfg: &config::FontConfig, px_size: f32) -> Option<font::FontFace> {
        let path = font_cfg.emoji_path.as_ref()?;
        match font::FontFace::load(path, px_size) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!(
                    "platform: failed to load emoji font {path:?}: {e} — emoji will render as tofu"
                );
                None
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
    pub const fn bg_xrgb(&self) -> u32 {
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
    /// premultiplied ARGB8888 otherwise. Delegates to the role-agnostic
    /// [`crate::surface::SurfaceBootstrap::pack_bg`], the single source of
    /// truth both this renderer and `statusbar::BarWindow` share.
    fn pack_bg(bg: DrawColor, bg_alpha: u8) -> u32 {
        crate::surface::SurfaceBootstrap::pack_bg(bg, bg_alpha)
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
        if cell.spacer() {
            return;
        }

        let (fg, bg) = self.theme.effective(&cell.attrs);
        self.blit_cell_with_colors(canvas, cx, cy, cell, fill_bg, fg, bg);
    }

    /// Same as [`Self::blit_cell_inner`], but takes an already-resolved
    /// `(fg, bg)` pair instead of calling `theme.effective` itself. Lets
    /// [`Self::blit_row`]'s glyph pass reuse the colors its background pass
    /// already computed per column, instead of resolving them twice per
    /// visible cell (a palette/indexed lookup plus, for bold/dim, an extra
    /// branch and lerp -- measurable on the `blit` bench's dense-cells
    /// corpus, where every cell has a distinct color).
    #[allow(clippy::too_many_arguments)]
    fn blit_cell_with_colors(
        &mut self,
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        cell: &Cell,
        fill_bg: bool,
        fg: DrawColor,
        bg: DrawColor,
    ) {
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
                Self::blit_cell_bitmap(canvas, cx, cy, cell.wide(), ch, fg_px, bg_px, fill_bg);
            }
            GlyphSource::Face(cache) => {
                Self::blit_cell_face(
                    canvas,
                    cache,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    cell.wide(),
                    ch,
                    fg,
                    bg,
                    self.bg_alpha,
                    fill_bg,
                );
            }
        }
        if cell.attrs.underline() {
            self.draw_underline(canvas, cx, cy, cell.attrs, fg);
        }
        if cell.attrs.strikethrough() {
            self.draw_strikethrough(canvas, cx, cy, cell.wide(), fg);
        }
    }

    /// Draw the underline decoration for one cell: a horizontal line (or,
    /// for [`grid::UnderlineStyle::Double`]/`Dotted`/`Dashed`, a small
    /// pattern of one) one pixel above the cell's bottom edge, in the
    /// glyph's own foreground unless `SGR 58` overrode it
    /// (`attrs.underline_color`). Runs after the glyph so it always sits on
    /// top, matching every other terminal's underline-over-descender look.
    fn draw_underline(
        &self,
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        attrs: Attrs,
        fg: DrawColor,
    ) {
        use grid::UnderlineStyle;
        let color = if matches!(attrs.underline_color, Color::Default) {
            fg
        } else {
            self.theme.resolve(attrs.underline_color, fg)
        };
        let px = if self.bg_alpha == 255 {
            color.to_xrgb8888()
        } else {
            color.premultiplied_argb8888(255)
        };
        let buf_w = canvas.width().cast_unsigned();
        let buf_h = canvas.height().cast_unsigned();
        let cell_w = self.cell_w.cast_unsigned();
        let cell_h = self.cell_h.cast_unsigned();
        let x0 = u32::from(cx) * cell_w;
        let y_base = u32::from(cy) * cell_h + cell_h.saturating_sub(1);
        if x0 >= buf_w || y_base >= buf_h {
            return;
        }
        let x1 = (x0 + cell_w).min(buf_w);
        let pixels = canvas.pixels_mut();
        let mut plot_row = |y: u32, dashed: bool| {
            if y >= buf_h {
                return;
            }
            let base = (y * buf_w) as usize;
            for (i, x) in (x0..x1).enumerate() {
                // Dashed/dotted: skip every other pixel (dotted) or every
                // third (dashed) for a visibly broken line; curly reuses the
                // solid single-row look here (a true sine-wave underline
                // needs sub-pixel row placement this simple integer path
                // doesn't have room for -- solid is still a clear, distinct
                // "styled" signal from `Single`).
                if dashed && i % 3 == 2 {
                    continue;
                }
                if let Some(p) = pixels.get_mut(base + x as usize) {
                    *p = px;
                }
            }
        };
        match attrs.underline_style() {
            UnderlineStyle::Double if y_base >= 1 => {
                plot_row(y_base, false);
                plot_row(y_base - 1, false);
            }
            UnderlineStyle::Dotted | UnderlineStyle::Dashed => plot_row(y_base, true),
            _ => plot_row(y_base, false),
        }
    }

    /// Draw the strikethrough decoration: a horizontal line through the
    /// cell's vertical middle, in the glyph's own (already-resolved)
    /// foreground. `wide` extends it across both columns of a wide glyph's
    /// cell pair, matching how the glyph itself spans them.
    fn draw_strikethrough(
        &self,
        canvas: &mut Canvas<'_>,
        cx: u16,
        cy: u16,
        wide: bool,
        fg: DrawColor,
    ) {
        let px = if self.bg_alpha == 255 {
            fg.to_xrgb8888()
        } else {
            fg.premultiplied_argb8888(255)
        };
        let buf_w = canvas.width().cast_unsigned();
        let buf_h = canvas.height().cast_unsigned();
        let cell_w = self.cell_w.cast_unsigned() * if wide { 2 } else { 1 };
        let cell_h = self.cell_h.cast_unsigned();
        let x0 = u32::from(cx) * self.cell_w.cast_unsigned();
        let y = u32::from(cy) * cell_h + cell_h / 2;
        if x0 >= buf_w || y >= buf_h {
            return;
        }
        let x1 = (x0 + cell_w).min(buf_w);
        let pixels = canvas.pixels_mut();
        let base = (y * buf_w) as usize;
        for x in x0..x1 {
            if let Some(p) = pixels.get_mut(base + x as usize) {
                *p = px;
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
        let cell_w = self.cell_w.cast_unsigned();
        let cell_h = self.cell_h.cast_unsigned();
        let buf_width = canvas.width().cast_unsigned();
        let py0 = u32::from(cy) * cell_h;

        if self.row_color_cache.len() < cells.len() {
            self.row_color_cache
                .resize(cells.len(), (self.theme.fg, self.theme.bg));
        }

        // ── Pass 1: batched background ──────────────────────────────────────
        {
            let mut c = 0usize;
            while c < cells.len() {
                let colors = self.theme.effective(&cells[c].attrs);
                self.row_color_cache[c] = colors;
                let base_bg = Self::pack_bg(colors.1, self.bg_alpha);
                let start = c;
                c += 1;
                // Extend the run while the background matches. A spacer column
                // (trailing half of a wide glyph) always belongs to its lead's
                // run, so it never breaks it -- this reproduces the old path,
                // where the wide lead filled both of its columns' backgrounds
                // and the spacer filled nothing.
                while c < cells.len() {
                    let next_bg = if cells[c].spacer() {
                        self.row_color_cache[c] = colors;
                        base_bg
                    } else {
                        let next_colors = self.theme.effective(&cells[c].attrs);
                        self.row_color_cache[c] = next_colors;
                        Self::pack_bg(next_colors.1, self.bg_alpha)
                    };
                    if next_bg != base_bg {
                        break;
                    }
                    c += 1;
                }

                // `start`/`c` are column indices bounded by `cells.len()`,
                // a terminal row width, always far under `u32::MAX`.
                let px_start = u32::try_from(start).unwrap_or(u32::MAX) * cell_w;
                if px_start >= buf_width {
                    continue;
                }
                let px_end = (u32::try_from(c).unwrap_or(u32::MAX) * cell_w).min(buf_width);
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
            // A blank space cell owes nothing once the background is down,
            // UNLESS it's underlined/struck-through (e.g. underlined
            // trailing whitespace from a search-highlight or a styled
            // prompt) -- those decorations still need painting even though
            // there's no glyph. `invisible()` forces a blank glyph AND
            // suppresses decorations (matches real terminals: invisible text
            // has no visible underline either).
            if cell.spacer() || cell.attrs.invisible() {
                continue;
            }
            if cell.ch == ' ' && !cell.attrs.underline() && !cell.attrs.strikethrough() {
                continue;
            }
            let (fg, bg) = self.row_color_cache[col];
            // `col` is a row index, always far under `u16::MAX`.
            self.blit_cell_with_colors(
                canvas,
                u16::try_from(col).unwrap_or(u16::MAX),
                cy,
                cell,
                false,
                fg,
                bg,
            );
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
        let buf_width = canvas.width().cast_unsigned();
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
        let buf_width = canvas.width().cast_unsigned();
        let bg_px = Self::pack_bg(bg, bg_alpha);
        let px0 = u32::from(cx) * cell_w.cast_unsigned();
        let py0 = u32::from(cy) * cell_h.cast_unsigned();
        let cell_cols: u32 = if wide { 2 } else { 1 };

        {
            let pixels = canvas.pixels_mut();
            // Precompute the clipped scanline width once (rather than a
            // per-pixel bounds check + index multiply) and fill each row
            // with a single slice `fill` -- the background is almost always
            // a solid rectangle, so this is the hot path for every cell
            // drawn.
            let cell_px_w = cell_w.cast_unsigned() * cell_cols;
            let clipped_w = if px0 >= buf_width {
                0
            } else {
                cell_px_w.min(buf_width - px0)
            };
            if fill_bg && clipped_w > 0 {
                for row in 0..cell_h.cast_unsigned() {
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
        // The atlas blend is the shared kernel (see `Canvas::blit_glyph`).
        // The glyph's pen origin is the cell's top-left (`px0`, `py0`); the
        // kernel adds `entry.bearing_x`/`bearing_y` -- `bearing_y` is the
        // already-resolved cell-top-to-glyph-top offset from
        // `FontFace::cell_bearing_y`, used directly (never recombined with a
        // separately-rounded ascender, which was the old baseline-jitter bug).
        canvas.blit_glyph(
            &cache.atlas,
            &entry,
            px0.cast_signed(),
            py0.cast_signed(),
            fg,
            bg,
            bg_alpha,
        );
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
        let buf_width = canvas.width().cast_unsigned();
        // Backbuffer pixel counts are always far under `u32::MAX` (a real
        // screen's pixel area), so this narrowing is infallible in practice.
        let buf_pixels = u32::try_from(canvas.pixels_mut().len()).unwrap_or(u32::MAX);
        let buf_rows = (buf_pixels / buf_width.max(1)) / self.cell_h.cast_unsigned();

        // Viewport (scrollback) pixel-scroll. See `blit_viewport_snapshot`'s
        // doc for the shift-vs-full-repaint decision this makes; overlay
        // imprints dragged by a shift are cleaned by the caller (reads
        // `snap.viewport_shift()`), so this always returns `None` (no
        // *incremental* scroll band to report) rather than a `ScrollBand`.
        if snap.viewport_offset().is_some() {
            self.blit_viewport_snapshot(canvas, snap, buf_width, buf_rows);
            return None;
        }

        // Explicit scroll band: the grid tells us exactly which rows moved up
        // and by how much, decoupled from which rows are dirty. Pixel-shift the
        // band, then fall through to paint the snapshot's precise dirty rows on
        // top -- those now sit at their true post-scroll positions, so an
        // in-place edit batched alongside a scroll is painted correctly instead
        // of being clobbered by a whole-region re-blit or a bad shift.
        let mut scrolled: Option<ScrollBand> = None;
        if snap.viewport_offset().is_none() {
            // An incremental frame is always the live bottom (new output snaps
            // the viewport to 0 before dirty rows accrue), so the backbuffer
            // now shows offset 0. Record it so the next scroll-back step shifts
            // from the correct baseline.
            self.last_viewport_offset = Some(0);
            if let Some(band) = snap.scroll_span() {
                let top = u32::from(band.top);
                let bot = u32::from(band.bot);
                let delta = u32::from(band.delta);
                if delta > 0 && delta <= bot - top && bot < buf_rows {
                    let row_scanlines = usize::try_from(self.cell_h).unwrap_or(0);
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

    /// The viewport (scrollback) half of [`Self::blit_snapshot`]: either
    /// `copy_within`-shift the backbuffer and rasterize only the freshly
    /// revealed edge rows, or -- when there's no usable prior offset, the
    /// step is too large to reuse anything, or new output changed the
    /// buffer contents (`content_delta != 0`) -- fully rasterize every row.
    ///
    /// Split out of `blit_snapshot` (which stays the single public entry
    /// point and decides *whether* this path applies) purely to keep that
    /// function's line count down; every field/branch here still reads as
    /// direct continuation of its caller's doc comment.
    fn blit_viewport_snapshot(
        &mut self,
        canvas: &mut Canvas<'_>,
        snap: &DirtySnapshot,
        buf_width: u32,
        buf_rows: u32,
    ) {
        let cur_offset =
            usize::from(snap.viewport_offset().expect(
                "blit_viewport_snapshot is only called when snap.viewport_offset() is Some",
            ));
        // Compute the pixel-shift from the RENDERER's own record of what
        // its backbuffer shows -- not the grid's `viewport_shift()` hint,
        // which is computed on the parser thread against an optimistic
        // `rendered_offset` that desyncs the moment a frame is requeued
        // (full ring) instead of delivered. `last_viewport_offset` only
        // advances when we actually blit below, so a dropped frame simply
        // leaves it where it was and the next delivered frame shifts by the
        // correct total. Gated on `content_delta == 0`: any new output
        // changed the buffer contents, so shifting the old pixels wouldn't
        // reproduce them (an output-driven snap-to-bottom must full-repaint).
        let shift = match self.last_viewport_offset {
            Some(prev) if snap.content_delta == 0 => {
                // Scrollback offsets are row counts, always far under
                // `i64::MAX`.
                let s = i64::try_from(prev).unwrap_or(0) - i64::try_from(cur_offset).unwrap_or(0);
                let mag_fits = u32::try_from(s.unsigned_abs()).is_ok_and(|m| m < buf_rows);
                (s != 0 && buf_rows > 0 && mag_fits).then(|| i32::try_from(s).unwrap_or(0))
            }
            _ => None,
        };
        if let Some(s) = shift {
            // Viewport (scrollback) pixel-scroll. A wheel/page step moves the
            // whole view by `shift` rows through history; the previous
            // frame's pixels are still valid, just at a new screen position.
            // `copy_within`-shift the entire backbuffer by that signed
            // amount and rasterize ONLY the freshly-revealed edge rows -- the
            // interior survives the memmove untouched. This is the fix for
            // the scroll-back fps cliff: the old path re-rasterized every
            // glyph on screen for each scroll frame.
            let row_words = buf_width as usize * usize::try_from(self.cell_h).unwrap_or(0);
            let mag = s.unsigned_abs() as usize;
            let keep = buf_rows as usize - mag;
            let pixels = canvas.pixels_mut();
            // s > 0: content moves up (view toward bottom) -- reveal at the
            // bottom, so lift rows [mag, buf_rows) to [0, keep).
            // s < 0: content moves down (view into history) -- reveal at the
            // top, so push rows [0, keep) down to [mag, buf_rows).
            if s > 0 {
                let src_off = mag * row_words;
                pixels.copy_within(src_off..src_off + keep * row_words, 0);
            } else {
                let dst_off = mag * row_words;
                pixels.copy_within(0..keep * row_words, dst_off);
            }
            // Rasterize only the revealed edge rows; the rest were shifted
            // into place by the memmove above.
            let (edge_lo, edge_hi) = if s > 0 {
                (buf_rows as usize - mag, buf_rows as usize)
            } else {
                (0usize, mag)
            };
            #[cfg(feature = "trace")]
            let mut rasterized = 0usize;
            for row in &snap.rows {
                let r = usize::from(row.row);
                if r >= edge_lo && r < edge_hi && u32::try_from(r).unwrap_or(u32::MAX) < buf_rows {
                    self.blit_row(canvas, row.row, &row.cells);
                    #[cfg(feature = "trace")]
                    {
                        rasterized += 1;
                    }
                }
            }
            self.last_viewport_offset = Some(cur_offset);
            #[cfg(feature = "trace")]
            {
                self.last_rasterized_rows = rasterized;
                self.last_viewport_shift = s;
            }
            return;
        }
        // Full-frame viewport repaint (first scroll, a jump too large to
        // reuse anything, or an output-driven snap-to-bottom): rasterize
        // every row.
        #[cfg(feature = "trace")]
        let mut rasterized = 0usize;
        for row in &snap.rows {
            if u32::from(row.row) >= buf_rows {
                continue;
            }
            self.blit_row(canvas, row.row, &row.cells);
            #[cfg(feature = "trace")]
            {
                rasterized += 1;
            }
        }
        self.last_viewport_offset = Some(cur_offset);
        #[cfg(feature = "trace")]
        {
            self.last_rasterized_rows = rasterized;
            self.last_viewport_shift = 0;
        }
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
    // cell_w/cell_h/buf_w/buf_h are pixel dimensions, always far below the
    // 2^24 exact-integer range of f32 -- no precision loss possible either
    // direction. The float->int casts below are all preceded by an explicit
    // `.floor()`/`.ceil()`/`.round()` and `.min`/`.max` clamp to the buffer
    // bounds, so the truncation is the intended rounding, not silent loss --
    // stable Rust has no checked float->int conversion API to express that
    // more precisely than `as` (see the crate's cast-policy note in
    // `font::raster` for the general rationale).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
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
    // Same rationale as `draw_cursor`: cell/buffer pixel dimensions never
    // approach f32's 2^24 exact-integer limit, and the truncating casts
    // below are all post-`.floor()`/`.ceil()`/`.round()` + clamp.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn draw_cursor_pulse(
        &self,
        canvas: &mut Canvas<'_>,
        visual_col: f32,
        visual_row: f32,
        t: f32,
    ) {
        const THICK: i32 = 2;

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
        Cell::new(ch, Attrs::default(), 0)
    }

    #[test]
    fn underline_paints_a_pixel_row_at_the_cell_bottom() {
        let mut renderer = Renderer::new(Theme::builtin(), &config::FontConfig::default());
        let mut buf = vec![0u32; usize::try_from(renderer.cell_w * renderer.cell_h).unwrap_or(0)];
        let mut canvas = Canvas::new(&mut buf, renderer.cell_w, renderer.cell_h);
        let mut attrs = Attrs::default();
        attrs.set_underline(true);
        let c = Cell::new('X', attrs, 0);
        renderer.blit_cell(&mut canvas, 0, 0, &c);
        let fg_px = renderer
            .theme
            .resolve(Color::Default, DrawColor::rgb(0xcc, 0xcc, 0xcc))
            .to_xrgb8888();
        let y = usize::try_from(renderer.cell_h - 1).unwrap_or(0);
        let cw = usize::try_from(renderer.cell_w).unwrap_or(0);
        let row_start = y * cw;
        let row = &buf[row_start..row_start + cw];
        assert!(
            row.iter().all(|&p| p == fg_px),
            "bottom row must be solid underline color"
        );
    }

    #[test]
    fn underline_color_override_is_used_instead_of_foreground() {
        let mut renderer = Renderer::new(Theme::builtin(), &config::FontConfig::default());
        let mut buf = vec![0u32; usize::try_from(renderer.cell_w * renderer.cell_h).unwrap_or(0)];
        let mut canvas = Canvas::new(&mut buf, renderer.cell_w, renderer.cell_h);
        let mut attrs = Attrs::default();
        attrs.set_underline(true);
        attrs.underline_color = grid::Color::Rgb(1, 2, 3);
        let c = Cell::new(' ', attrs, 0);
        renderer.blit_cell(&mut canvas, 0, 0, &c);
        let expect = DrawColor::rgb(1, 2, 3).to_xrgb8888();
        let y = usize::try_from(renderer.cell_h - 1).unwrap_or(0);
        let row_start = y * usize::try_from(renderer.cell_w).unwrap_or(0);
        assert_eq!(buf[row_start], expect);
    }

    #[test]
    fn strikethrough_paints_a_pixel_row_through_the_middle() {
        let mut renderer = Renderer::new(Theme::builtin(), &config::FontConfig::default());
        let mut buf = vec![0u32; usize::try_from(renderer.cell_w * renderer.cell_h).unwrap_or(0)];
        let mut canvas = Canvas::new(&mut buf, renderer.cell_w, renderer.cell_h);
        let mut attrs = Attrs::default();
        attrs.set_strikethrough(true);
        let c = Cell::new('X', attrs, 0);
        renderer.blit_cell(&mut canvas, 0, 0, &c);
        let fg_px = renderer
            .theme
            .resolve(Color::Default, DrawColor::rgb(0xcc, 0xcc, 0xcc))
            .to_xrgb8888();
        let y = usize::try_from(renderer.cell_h / 2).unwrap_or(0);
        let cw = usize::try_from(renderer.cell_w).unwrap_or(0);
        let row_start = y * cw;
        let row = &buf[row_start..row_start + cw];
        assert!(
            row.iter().all(|&p| p == fg_px),
            "middle row must be solid strikethrough color"
        );
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

        let attr_cell = |ch: char, a: &Attrs, wide: bool, spacer: bool| {
            if wide {
                Cell::wide_lead(ch, *a, 0)
            } else if spacer {
                Cell::wide_spacer(*a, 0)
            } else {
                Cell::new(ch, *a, 0)
            }
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

        let cols = u32::try_from(cells.len()).unwrap_or(0);
        let bw = cols * GLYPH_W as u32;
        let bh = GLYPH_H as u32;

        // Reference: force the old per-cell path (fill_bg = true for every
        // cell, no batching, no blank-skip).
        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut canvas = Canvas::new(&mut reference, bw.cast_signed(), bh.cast_signed());
            for (col, c) in cells.iter().enumerate() {
                r.blit_cell_inner(&mut canvas, u16::try_from(col).unwrap_or(0), 0, c, true);
            }
        }

        // Batched path under test.
        let mut batched = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut canvas = Canvas::new(&mut batched, bw.cast_signed(), bh.cast_signed());
            r.blit_row(&mut canvas, 0, &cells);
        }

        assert_eq!(
            batched, reference,
            "batched blit_row diverged from per-cell"
        );
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
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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
            let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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
        let bw = u32::from(cols) * GLYPH_W as u32;
        let bh = u32::from(rows) * GLYPH_H as u32;

        // Reference renderer: full re-blit of the final state (delta forced 0
        // so it never takes the fast path).
        let final_base = 100u32;
        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let mut full = scroll_snap(cols, rows, final_base, 0);
            full.content_delta = 0;
            let mut canvas = Canvas::new(&mut reference, bw.cast_signed(), bh.cast_signed());
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
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
            r.blit_snapshot(&mut canvas, &seed);
        }
        // Now scroll to final in steps, each a fast-path snapshot.
        let mut acc = 0u32;
        {
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
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

    // A full-frame viewport (scrollback) snapshot: every visible row shows a
    // slice of history starting at logical line `base_line`, at scrollback
    // `offset`. The renderer derives the pixel-shift from consecutive frames'
    // `offset`s (via its own `last_viewport_offset`), so the two frames of a
    // scroll must carry offsets whose difference equals the intended shift --
    // exactly as the live grid emits them (`shift = prev_offset - cur_offset`).
    // `shift` is retained only to mirror what the grid would stamp; the
    // renderer no longer reads it.
    fn viewport_snap(
        cols: u16,
        rows: u16,
        base_line: u32,
        offset: u16,
        shift: Option<i32>,
    ) -> DirtySnapshot {
        let dirty = (0..rows)
            .map(|r| {
                let line = base_line + u32::from(r);
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
            content_delta: 0,
            kind: grid::SnapshotKind::Viewport { offset, shift },
        }
    }

    #[test]
    fn viewport_pixel_shift_up_into_history_matches_full_repaint() {
        // Scrolling up into history reveals rows at the TOP (shift < 0). The
        // fast signed copy_within + edge-row rasterization must be byte-
        // identical to a full repaint of the same revealed content.
        let cols = 4u16;
        let rows = 8u16;
        let bw = u32::from(cols) * GLYPH_W as u32;
        let bh = u32::from(rows) * GLYPH_H as u32;

        // Showing lines [50,57]; scroll up into history -> show [47,54].
        // View moves down on screen (reveal at top) => shift = -3. Scrolling up
        // into history RAISES the offset, so the final frame's offset is the
        // seed's + 3 (renderer computes shift = prev - cur = -3).
        let seed_base = 50u32;
        let step = 3u32;
        let final_base = seed_base - step;
        let seed_offset = 10u16;
        let final_offset = seed_offset + u16::try_from(step).unwrap_or(0);

        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let full = viewport_snap(cols, rows, final_base, final_offset, None);
            let mut canvas = Canvas::new(&mut reference, bw.cast_signed(), bh.cast_signed());
            assert_eq!(r.blit_snapshot(&mut canvas, &full), None);
        }

        let mut fast = vec![0u32; (bw * bh) as usize];
        let mut r = Renderer::builtin(Theme::builtin());
        {
            let seed = viewport_snap(cols, rows, seed_base, seed_offset, None);
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
            r.blit_snapshot(&mut canvas, &seed);
        }
        {
            let snap = viewport_snap(
                cols,
                rows,
                final_base,
                final_offset,
                Some(-step.cast_signed()),
            );
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
            assert_eq!(r.blit_snapshot(&mut canvas, &snap), None);
        }

        assert_eq!(
            fast, reference,
            "viewport up-scroll shift diverged from full repaint"
        );
    }

    #[test]
    fn viewport_pixel_shift_down_toward_bottom_matches_full_repaint() {
        // Scrolling back toward the live bottom reveals rows at the BOTTOM
        // (shift > 0). Same byte-identity requirement, opposite direction.
        let cols = 4u16;
        let rows = 8u16;
        let bw = u32::from(cols) * GLYPH_W as u32;
        let bh = u32::from(rows) * GLYPH_H as u32;

        // Scrolling back toward the bottom LOWERS the offset, so the final
        // frame's offset is the seed's - 3 (renderer computes shift = +3).
        let seed_base = 47u32;
        let step = 3u32;
        let final_base = seed_base + step;
        let seed_offset = 10u16;
        let final_offset = seed_offset - u16::try_from(step).unwrap_or(0);

        let mut reference = vec![0u32; (bw * bh) as usize];
        {
            let mut r = Renderer::builtin(Theme::builtin());
            let full = viewport_snap(cols, rows, final_base, final_offset, None);
            let mut canvas = Canvas::new(&mut reference, bw.cast_signed(), bh.cast_signed());
            r.blit_snapshot(&mut canvas, &full);
        }

        let mut fast = vec![0u32; (bw * bh) as usize];
        let mut r = Renderer::builtin(Theme::builtin());
        {
            let seed = viewport_snap(cols, rows, seed_base, seed_offset, None);
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
            r.blit_snapshot(&mut canvas, &seed);
        }
        {
            let snap = viewport_snap(
                cols,
                rows,
                final_base,
                final_offset,
                Some(step.cast_signed()),
            );
            let mut canvas = Canvas::new(&mut fast, bw.cast_signed(), bh.cast_signed());
            r.blit_snapshot(&mut canvas, &snap);
        }

        assert_eq!(
            fast, reference,
            "viewport down-scroll shift diverged from full repaint"
        );
    }

    #[test]
    fn viewport_full_repaint_when_shift_is_none() {
        // A viewport snapshot with shift=None (first scroll, or a jump too big
        // to reuse anything) must repaint every row and report no scroll band.
        let cols = 3u16;
        let rows = 5u16;
        let bw = u32::from(cols) * GLYPH_W as u32;
        let bh = u32::from(rows) * GLYPH_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];
        let mut r = Renderer::builtin(Theme::builtin());
        let snap = viewport_snap(cols, rows, 20, 20, None);
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
        assert_eq!(r.blit_snapshot(&mut canvas, &snap), None);
        assert!(
            pixels.iter().any(|&p| p != 0),
            "full repaint painted nothing"
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
            let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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

    // cols/rows are tiny test fixture values, nowhere near f32's precision limit.
    #[allow(clippy::cast_precision_loss)]
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
        let mut canvas = Canvas::new(&mut pixels, bw.cast_signed(), bh.cast_signed());
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

    #[test]
    fn effective_plain_fg_is_the_resolved_color() {
        let theme = Theme::builtin();
        let mut a = Attrs::default();
        a.fg = grid::Color::Rgb(200, 100, 50);
        let (fg, _bg) = theme.effective(&a);
        assert_eq!(fg, DrawColor::rgb(200, 100, 50));
    }

    #[test]
    fn effective_dim_pulls_fg_toward_bg() {
        // Dim (SGR 2) must land the fg strictly between bg and the full fg --
        // fish pagers / `ls` metadata rely on faint text being visibly muted
        // but not invisible. Pure-white fg on pure-black bg makes the
        // direction unambiguous.
        let mut theme = Theme::builtin();
        theme.fg = DrawColor::rgb(255, 255, 255);
        theme.bg = DrawColor::rgb(0, 0, 0);
        let mut a = Attrs::default(); // fg/bg default -> theme fg/bg
        let (full, _) = theme.effective(&a);
        assert_eq!(full, DrawColor::rgb(255, 255, 255));
        a.set_dim(true);
        let (faint, _) = theme.effective(&a);
        assert!(
            faint.r > 0 && faint.r < 255,
            "dim fg must be a muted grey, got {faint:?}"
        );
        assert!(faint.r < full.r, "dim must darken toward bg");
    }

    #[test]
    fn effective_bold_promotes_low_palette_to_bright() {
        // Bold (SGR 1) on a palette color 0..=7 promotes it to the bright
        // 8..=15 slot -- the classic xterm "bold is bright". Index 1 (red)
        // -> index 9 (bright red).
        let theme = Theme::builtin();
        let mut a = Attrs::default();
        a.fg = grid::Color::Indexed(1);
        a.set_bold(true);
        let (fg, _) = theme.effective(&a);
        let (r, g, b) = theme.ansi[9];
        assert_eq!(fg, DrawColor::rgb(r, g, b));
    }

    #[test]
    fn effective_bold_leaves_truecolor_fg_untouched() {
        // Only palette indices 0..=7 get brightened; a truecolor fg has no
        // "bright" sibling, so bold must leave it exactly as-is.
        let theme = Theme::builtin();
        let mut a = Attrs::default();
        a.fg = grid::Color::Rgb(10, 20, 30);
        a.set_bold(true);
        let (fg, _) = theme.effective(&a);
        assert_eq!(fg, DrawColor::rgb(10, 20, 30));
    }

    #[test]
    fn effective_brblack_autosuggestion_is_visible_against_bg() {
        // fish paints its inline autosuggestions in `brblack` (SGR 90 ->
        // palette index 8), NOT dim. Whatever else is true, that color must
        // resolve to something distinct from the background or the suggestion
        // is invisible. Check both the builtin theme and the config default
        // palette (the two backgrounds a real run can have).
        for theme in [
            Theme::builtin(),
            Theme::from_palette(&config::Palette::default()),
        ] {
            let mut a = Attrs::default();
            a.fg = grid::Color::Indexed(8); // brblack
            let (fg, bg) = theme.effective(&a);
            assert_ne!(fg, bg, "brblack autosuggestion fg must differ from bg");
        }
    }

    /// End-to-end reproduction of the interactive scroll-back path: a real
    /// `Grid` drives a real `Renderer` through the exact live sequence
    /// (`scroll_viewport` -> `take_dirty` -> `blit_snapshot`) that the present
    /// loop runs each frame. Asserts the pixel-shift ENGAGES -- i.e. each
    /// small wheel step rasterizes only the freshly revealed edge rows, not
    /// the whole screen. This is the headless equivalent of watching the
    /// `RASTERIZED N shift S` debug line while wheeling through history, so we
    /// never have to eyeball it in a live run again. Needs the `trace`
    /// feature, which compiles in the per-frame rasterized-row counters.
    #[cfg(feature = "trace")]
    #[test]
    fn scrollback_wheel_step_pixel_shifts_instead_of_full_repaint() {
        use grid::Grid;

        let cols = 40u16;
        let rows = 24u16;
        let mut g = Grid::new(cols, rows);
        // Prime a deep scrollback, exactly like the live session.
        let mut p = vt_parser::Parser::new();
        for i in 0..400 {
            let line = format!("line {i}: the quick brown fox jumps over 0123456789\r\n");
            p.feed(line.as_bytes(), |ev| g.apply(&ev));
        }
        // Drain the output-driven frame so we start clean at offset 0 with
        // rendered_offset = Some(0) -- the baseline a wheel-scroll shifts from.
        let mut renderer = Renderer::builtin(Theme::builtin());
        let cell_w = usize::try_from(renderer.cell_w()).unwrap_or(0);
        let cell_h = usize::try_from(renderer.cell_h()).unwrap_or(0);
        let bw = cols as usize * cell_w;
        let bh = rows as usize * cell_h;
        let mut backbuffer = vec![0u32; bw * bh];
        let signed_w = i32::try_from(bw).unwrap_or(i32::MAX);
        let signed_h = i32::try_from(bh).unwrap_or(i32::MAX);

        // Blit the output-driven frame (as the present loop does) so the
        // renderer's `last_viewport_offset` is primed to Some(0) -- the live
        // baseline a wheel-scroll shifts away from.
        {
            let snap = g.take_dirty();
            let mut canvas = Canvas::new(&mut backbuffer, signed_w, signed_h);
            renderer.blit_snapshot(&mut canvas, &snap);
        }

        // Every wheel step should now pixel-shift, rasterizing only the single
        // revealed edge row, never the full `rows` set.
        for step in 0..8 {
            g.scroll_viewport(1);
            let snap = g.take_dirty();
            assert_eq!(
                snap.viewport_shift(),
                Some(-1),
                "step {step}: a one-line wheel step must emit shift -1"
            );
            {
                let mut canvas = Canvas::new(&mut backbuffer, signed_w, signed_h);
                renderer.blit_snapshot(&mut canvas, &snap);
            }
            assert_eq!(
                renderer.last_viewport_shift, -1,
                "step {step}: blit must take the pixel-shift branch"
            );
            assert_eq!(
                renderer.last_rasterized_rows, 1,
                "step {step}: only the one revealed edge row may be rasterized, \
                 not the whole screen ({rows} rows)"
            );
        }
    }

    /// Regression: `requeue_snapshot` (fired when the SPSC dirty ring is full,
    /// i.e. the UI thread is behind -- which it IS at 3ms/frame during a TTF
    /// full repaint) used to force a full repaint every frame, because the
    /// grid's optimistic `rendered_offset` got reset to `None` on requeue.
    /// That made a self-sustaining cliff: full repaint -> UI lags -> ring full
    /// -> requeue -> full repaint... matching "stays slow while parked, only
    /// recovers at the bottom." The fix moved offset ownership into the
    /// renderer (`last_viewport_offset`), which only advances when a frame is
    /// ACTUALLY blitted -- so a requeued (undelivered) frame can't desync it.
    /// This test now asserts the shift STILL engages under a full-ring storm.
    /// Needs the `trace` feature for the per-frame rasterized-row counters.
    #[cfg(feature = "trace")]
    #[test]
    fn scrollback_requeue_every_frame_defeats_the_pixel_shift() {
        use grid::Grid;
        let cols = 40u16;
        let rows = 24u16;
        let mut g = Grid::new(cols, rows);
        let mut p = vt_parser::Parser::new();
        for i in 0..400 {
            let line = format!("line {i}: the quick brown fox jumps over 0123456789\r\n");
            p.feed(line.as_bytes(), |ev| g.apply(&ev));
        }
        let mut renderer = Renderer::builtin(Theme::builtin());
        let cell_w = usize::try_from(renderer.cell_w()).unwrap_or(0);
        let cell_h = usize::try_from(renderer.cell_h()).unwrap_or(0);
        let bw = cols as usize * cell_w;
        let bh = rows as usize * cell_h;
        let mut backbuffer = vec![0u32; bw * bh];
        let signed_w = i32::try_from(bw).unwrap_or(i32::MAX);
        let signed_h = i32::try_from(bh).unwrap_or(i32::MAX);

        // Blit the output-driven frame so the renderer's offset is primed to
        // Some(0) -- the live baseline (present loop always blits it).
        {
            let snap = g.take_dirty();
            let mut canvas = Canvas::new(&mut backbuffer, signed_w, signed_h);
            renderer.blit_snapshot(&mut canvas, &snap);
        }

        let mut shifted = 0;
        for _ in 0..8 {
            g.scroll_viewport(1);
            let snap = g.take_dirty();
            // Simulate the ring being full: the parser requeues the snapshot it
            // just built instead of the UI consuming it. The grid's own hint is
            // now distrusted, but the RENDERER's offset is untouched (nothing
            // was blitted), so the eventually-delivered frame still shifts.
            g.requeue_snapshot(&snap);
            // Next frame the parser rebuilds and (this time) delivers it.
            let snap = g.take_dirty();
            {
                let mut canvas = Canvas::new(&mut backbuffer, signed_w, signed_h);
                renderer.blit_snapshot(&mut canvas, &snap);
            }
            // Each delivered frame advanced the viewport by one net line since
            // the last blit, so the renderer shifts by -1 -- never a full
            // repaint, no matter how many frames were requeued in between.
            if renderer.last_viewport_shift == -1 && renderer.last_rasterized_rows == 1 {
                shifted += 1;
            }
        }
        assert_eq!(
            shifted, 8,
            "FIX: renderer-owned offset keeps the pixel-shift engaged even when \
             every frame is requeued (ring full)"
        );
    }

    #[test]
    fn effective_inverse_still_swaps_fg_and_bg() {
        let mut theme = Theme::builtin();
        theme.fg = DrawColor::rgb(1, 2, 3);
        theme.bg = DrawColor::rgb(9, 8, 7);
        let mut a = Attrs::default();
        a.set_inverse(true);
        let (fg, bg) = theme.effective(&a);
        assert_eq!(fg, DrawColor::rgb(9, 8, 7));
        assert_eq!(bg, DrawColor::rgb(1, 2, 3));
    }
}
