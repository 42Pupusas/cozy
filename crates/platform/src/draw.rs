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

/// An RGBA color.
///
/// Alpha is ignored by the XRGB8888 paths (`fill_rect`, `clear`), which pack
/// `0x00RRGGBB`, and honoured by the ARGB ones ([`Canvas::fill_rect_argb`],
/// [`Color::to_argb8888`]) used for surfaces with genuine per-pixel
/// transparency.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Every one of `config::Color`'s 24-bit RGB values is fully opaque -- the
/// config file format has no per-color alpha concept (window/bar/monitor
/// opacity are separate scalar fields) -- so this conversion always produces
/// `a: 0xff`, same as [`Color::rgb`].
///
/// Was three call sites (`bins/monitor`, `bins/statusbar`, `render.rs`) each
/// hand-rolling `Color::rgb(c.r, c.g, c.b)`; a `From` impl is the idiomatic
/// single place for a conversion this mechanical, and lets callers write
/// `.into()` instead of repeating the field-by-field destructuring.
impl From<config::Color> for Color {
    fn from(c: config::Color) -> Self {
        Self::rgb(c.r, c.g, c.b)
    }
}

impl Color {
    /// Opaque color from 8-bit channels.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 0xff }
    }

    /// Color with an explicit alpha channel.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Pack into the `0x00RRGGBB` word the XRGB8888 buffer expects.
    #[must_use]
    pub const fn to_xrgb8888(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Pack into the `0xAARRGGBB` word the ARGB8888 format expects (cursors,
    /// subsurfaces with per-pixel alpha).
    #[must_use]
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

    /// Composite an anti-aliased glyph edge pixel: `glyph_a/255` coverage of
    /// opaque ink (`self`, the foreground) over a `bg` background that is
    /// itself translucent at `bg_alpha/255` (the window opacity). Returns a
    /// plain packed RGB word (top byte ignored, i.e. XRGB8888) when
    /// `bg_alpha == 255` -- byte-identical to the original (pre-M10) blend --
    /// or a **premultiplied** ARGB8888 word otherwise, per `wl_shm`'s wire
    /// format.
    ///
    /// Was a free `blend(bg, fg, glyph_a, bg_alpha)` function in
    /// `render.rs` -- moved onto `Color` (the foreground ink) since it's
    /// fundamentally a color-compositing operation, not a renderer concern.
    #[must_use]
    pub fn blend_glyph(self, bg: Self, glyph_a: u8, bg_alpha: u8) -> u32 {
        let fg = self;
        if bg_alpha == 255 {
            return bg.blend_opaque_to(fg, glyph_a);
        }
        // Ink is opaque; the window glass shows through the uncovered fraction.
        // Porter-Duff "ink over glass", composited directly in premultiplied space.
        let ink_a = f64::from(glyph_a) / 255.0;
        let glass_a = f64::from(bg_alpha) / 255.0;
        let uncovered = glass_a * (1.0 - ink_a);
        let out_a = ink_a + uncovered;
        let pr = f64::from(fg.r).mul_add(ink_a, f64::from(bg.r) * uncovered);
        let pg = f64::from(fg.g).mul_add(ink_a, f64::from(bg.g) * uncovered);
        let pb = f64::from(fg.b).mul_add(ink_a, f64::from(bg.b) * uncovered);
        // Float source, no lossless-widening path available (see the font
        // crate's raster.rs doc for the full float<->int cast rationale) --
        // these are packed 8-bit color channels, always in 0.0..=255.0.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (a8, r8, g8, b8) = (
            (out_a * 255.0).round() as u32,
            pr.round() as u32,
            pg.round() as u32,
            pb.round() as u32,
        );
        (a8 << 24) | (r8 << 16) | (g8 << 8) | b8
    }

    /// The original (pre-transparency) linear-interpolate `self` to `to` by
    /// `alpha/255`, packed as XRGB8888. Split out of [`Color::blend_glyph`]
    /// so the opaque path (the overwhelmingly common case) stays exactly as
    /// before.
    ///
    /// Was a free `blend_opaque(from, to, alpha)` function in `render.rs`.
    #[must_use]
    pub fn blend_opaque_to(self, to: Self, alpha: u8) -> u32 {
        self.blend_opaque_color(to, alpha).to_xrgb8888()
    }

    /// Return the opaque colour produced by compositing `to` over `self`.
    /// This keeps the result reusable when an entire primitive has the same
    /// known background, as with a monitor chart's freshly painted panel.
    #[must_use]
    pub fn blend_opaque_color(self, to: Self, alpha: u8) -> Self {
        let from = self;
        let a = u32::from(alpha);
        // `div255(x)` == `x / 255` exactly for any u32 in `[0, 255*255]` (the
        // range these lerp numerators occupy), but with a multiply+shift
        // instead of a hardware divide. This is the anti-aliased glyph-edge
        // blend, ~21% of the real Face-path flood (perf) -- three divides per
        // AA pixel were the single most expensive op in that frame.
        let div255 = |x: u32| (x + 1 + (x >> 8)) >> 8;
        #[allow(clippy::cast_possible_truncation)]
        Self::rgb(
            div255(u32::from(from.r) * (255 - a) + u32::from(to.r) * a) as u8,
            div255(u32::from(from.g) * (255 - a) + u32::from(to.g) * a) as u8,
            div255(u32::from(from.b) * (255 - a) + u32::from(to.b) * a) as u8,
        )
    }
}

/// A handful of named colors so call sites read in words, not hex.
impl Color {
    pub const BLACK: Self = Self::rgb(0x00, 0x00, 0x00);
    pub const WHITE: Self = Self::rgb(0xff, 0xff, 0xff);
    pub const SLATE: Self = Self::rgb(0x1e, 0x1e, 0x28); // the window background
    pub const CORAL: Self = Self::rgb(0xff, 0x5c, 0x57); // the box
    pub const TEAL: Self = Self::rgb(0x57, 0xc7, 0xff);
    /// Fully transparent black — the zero value in ARGB8888.
    pub const TRANSPARENT: Self = Self::rgba(0x00, 0x00, 0x00, 0x00);
}

/// A point in pixel space. Signed so it can sit off any edge.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[must_use]
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
    #[must_use]
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// Exclusive right/bottom edges.
    #[must_use]
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    #[must_use]
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Shrink the rect inward by `n` on every side (negative `n` grows it).
    #[must_use]
    pub const fn inset(&self, n: i32) -> Self {
        Self::new(self.x + n, self.y + n, self.w - 2 * n, self.h - 2 * n)
    }

    /// Whether `p` lies inside the rect (left/top inclusive, right/bottom
    /// exclusive — matching how `fill_rect` paints).
    #[must_use]
    pub const fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
}

/// The set of pixel rectangles a custom painter actually drew this frame.
///
/// A translucent custom-painted window must premultiply its backbuffer into the
/// ARGB cache before upload. Doing that over the whole surface when a
/// steady-state frame changes a tiny fraction of it is the dominant per-frame
/// cost (measured at tens of milliseconds for a full-HD monitor dashboard).
/// A painter that knows exactly which regions it touched returns them here so
/// [`crate::window::TermWindow`] premultiplies and uploads only those.
///
/// `Full` means "treat the whole window as damaged" (a full repaint, or a
/// painter that doesn't track regions); an empty `Regions` means nothing
/// changed. The rectangles are in window pixel coordinates.
#[derive(Clone, Debug, Default)]
pub struct Damage {
    regions: Vec<Rect>,
    full: bool,
}

impl Damage {
    /// Nothing was drawn.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            regions: Vec::new(),
            full: false,
        }
    }

    /// The whole window is damaged (full repaint / non-tracking painter).
    #[must_use]
    pub const fn full() -> Self {
        Self {
            regions: Vec::new(),
            full: true,
        }
    }

    /// Damage described by an explicit set of rectangles.
    #[must_use]
    pub const fn regions(rects: Vec<Rect>) -> Self {
        Self {
            regions: rects,
            full: false,
        }
    }

    /// Whether the whole window should be treated as damaged.
    #[must_use]
    pub const fn is_full(&self) -> bool {
        self.full
    }

    /// Whether nothing was damaged (no regions and not full).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.full && self.regions.is_empty()
    }

    /// The damaged rectangles (empty when `full` or nothing changed).
    #[must_use]
    pub fn rects(&self) -> &[Rect] {
        &self.regions
    }

    /// The bounding box over all damage, or an empty rect if none. `full`
    /// callers should special-case before calling this.
    #[must_use]
    pub fn bounding(&self) -> Rect {
        let mut it = self.regions.iter();
        let Some(&first) = it.next() else {
            return Rect::new(0, 0, 0, 0);
        };
        let (mut x0, mut y0) = (first.x, first.y);
        let (mut x1, mut y1) = (first.right(), first.bottom());
        for r in it {
            x0 = x0.min(r.x);
            y0 = y0.min(r.y);
            x1 = x1.max(r.right());
            y1 = y1.max(r.bottom());
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
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
        // Checked non-negative above, so these sign-preserving casts are exact.
        debug_assert!(
            pixels.len() >= (width.cast_unsigned() as usize) * (height.cast_unsigned() as usize)
        );
        Self {
            pixels,
            width,
            height,
        }
    }

    #[must_use]
    pub const fn width(&self) -> i32 {
        self.width
    }
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// The raw pixel buffer, mutable. Escape hatch for other modules (e.g.
    /// `render`'s grid-cell blitting) that need direct flat-index access
    /// rather than one of `Canvas`'s coordinate-based drawing primitives.
    pub const fn pixels_mut(&mut self) -> &mut [u32] {
        self.pixels
    }

    /// The whole canvas as a rect — handy for full-surface ops.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
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
        // Checked non-negative (and in-bounds) above, so these
        // sign-preserving casts are exact.
        let idx = (y.cast_unsigned() as usize) * (self.width.cast_unsigned() as usize)
            + (x.cast_unsigned() as usize);
        self.pixels[idx] = color.to_xrgb8888();
    }

    /// Alpha-blend opaque `color` over one canvas pixel.
    ///
    /// Canvas surfaces are XRGB, so the result is written back as an opaque
    /// color. This is intended for software anti-aliasing and subtle fills.
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let idx = (y.cast_unsigned() as usize) * (self.width.cast_unsigned() as usize)
            + (x.cast_unsigned() as usize);
        let packed = self.pixels[idx];
        let background = Color::rgb(
            ((packed >> 16) & 0xff) as u8,
            ((packed >> 8) & 0xff) as u8,
            (packed & 0xff) as u8,
        );
        self.pixels[idx] = background.blend_opaque_to(color, alpha);
    }

    /// Fill `rect` (clipped to the canvas) with `color`, fully opaque.
    ///
    /// Packs `color` as plain `0x00RRGGBB` (top byte zero). That is correct
    /// only when the destination buffer is genuinely `Xrgb8888` (the
    /// compositor ignores the top byte) -- on a translucent window's
    /// `Argb8888` backbuffer the top byte **is** the alpha channel the
    /// compositor reads, so this call would paint an invisible (alpha-0)
    /// rectangle. Use [`Self::fill_rect_alpha`] for anything drawn onto a
    /// window that might be translucent (any `bg_alpha < 255`); this
    /// opaque-only entry point stays for buffers that are always Xrgb8888
    /// (e.g. an always-opaque widget) and for the tests below.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.fill_rect_packed(rect, color.to_xrgb8888());
    }

    /// Fill `rect` (clipped to the canvas) with `color`, packed the same way
    /// glyph ink is in [`Self::blit_glyph`]: plain `0x00RRGGBB` when
    /// `bg_alpha == 255` (the fast, common opaque-window path, byte-
    /// identical to [`Self::fill_rect`]), or **premultiplied** ARGB8888 at
    /// full alpha (solid ink -- `premultiplied_argb8888(255)` is the
    /// identity) otherwise, so the fill is fully opaque paint over a
    /// translucent "glass" window instead of itself carrying the window's
    /// alpha. Mirrors `platform::Color`'s "translucent glass, solid text"
    /// contract for any solid-color UI chrome (selection highlights, etc.),
    /// not just glyphs.
    pub fn fill_rect_alpha(&mut self, rect: Rect, color: Color, bg_alpha: u8) {
        let packed = if bg_alpha == 255 {
            color.to_xrgb8888()
        } else {
            color.premultiplied_argb8888(255)
        };
        self.fill_rect_packed(rect, packed);
    }

    /// Fill `rect` with `color` honouring **`color`'s own alpha channel**,
    /// packed premultiplied as `wl_shm` requires.
    ///
    /// The distinction from [`Self::fill_rect_alpha`] matters: that one paints
    /// solid ink *over* a translucent window (alpha is a property of the
    /// window, and the fill is always opaque). This one treats alpha as a
    /// property of the *paint*, which is what a surface with genuine
    /// per-pixel transparency needs -- a selection overlay that dims part of
    /// the screen and leaves the rest see-through, a cursor, a shaped
    /// subsurface. Filling such a surface with [`Self::fill_rect`] silently
    /// zeroes the alpha byte and yields a fully transparent (invisible)
    /// rectangle.
    pub fn fill_rect_argb(&mut self, rect: Rect, color: Color) {
        self.fill_rect_packed(rect, color.premultiplied_argb8888(color.a));
    }

    /// Shared clip + row-fill loop for [`Self::fill_rect`]/[`Self::fill_rect_alpha`].
    fn fill_rect_packed(&mut self, rect: Rect, v: u32) {
        let Some((x0, y0, x1, y1)) = self.clip(rect) else {
            return;
        };
        // `self.width` is always non-negative (checked in Canvas::new), and
        // `clip` guarantees `y0/y1/x0/x1` are all non-negative in-bounds
        // coordinates, so these sign-preserving casts are exact.
        let w = self.width.cast_unsigned() as usize;
        for y in y0..y1 {
            let row = (y.cast_unsigned() as usize) * w;
            let start = row + x0.cast_unsigned() as usize;
            let end = row + x1.cast_unsigned() as usize;
            for px in &mut self.pixels[start..end] {
                *px = v;
            }
        }
    }

    /// Copy tightly packed XRGB pixels into `rect`, clipping to the canvas.
    ///
    /// This is the software equivalent of the terminal renderer's persistent
    /// backbuffer blit: a cached widget can update privately, then transfer its
    /// finished rows without rerasterizing its contents in the parent canvas.
    pub fn copy_pixels(&mut self, rect: Rect, source: &[u32]) {
        if rect.w <= 0 || rect.h <= 0 {
            return;
        }
        let required = rect.w.cast_unsigned() as usize * rect.h.cast_unsigned() as usize;
        if source.len() < required {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clip(rect) else {
            return;
        };
        let source_width = rect.w.cast_unsigned() as usize;
        let target_width = self.width.cast_unsigned() as usize;
        for y in y0..y1 {
            let source_y = (y - rect.y).cast_unsigned() as usize;
            let source_x = (x0 - rect.x).cast_unsigned() as usize;
            let source_start = source_y * source_width + source_x;
            let copy_width = (x1 - x0).cast_unsigned() as usize;
            let target_start =
                y.cast_unsigned() as usize * target_width + x0.cast_unsigned() as usize;
            self.pixels[target_start..target_start + copy_width]
                .copy_from_slice(&source[source_start..source_start + copy_width]);
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
        if (u32::from(font::FIRST)..=u32::from(font::LAST)).contains(&code) {
            // Checked within FIRST..=LAST above (both u8-range), so this
            // always fits in u8.
            let code_u8 = u8::try_from(code).expect("checked code fits in u8");
            let glyph = &font::FONT[(code_u8 - font::FIRST) as usize];
            for (row, bits) in glyph.iter().enumerate() {
                // MSB is the leftmost pixel.
                for col in 0..font::GLYPH_W {
                    if bits & (0x80 >> col) != 0 {
                        // A glyph is always a handful of rows, well under i32::MAX.
                        let row_i32 = i32::try_from(row).unwrap_or(i32::MAX);
                        self.set_pixel(x + col, y + row_i32, color);
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
        let (mut left, mut right) = (x0.min(x1), x0.max(x1));
        left = left.max(0);
        right = right.min(self.width - 1);
        if left > right {
            return;
        }
        let packed = color.to_xrgb8888();
        // `y` is checked in-bounds (so non-negative) above, and
        // `self.width` is always non-negative (checked in Canvas::new), so
        // these sign-preserving casts are exact.
        let row = (y.cast_unsigned() as usize) * (self.width.cast_unsigned() as usize);
        let start = row + left.cast_unsigned() as usize;
        let end = row + right.cast_unsigned() as usize;
        for px in &mut self.pixels[start..=end] {
            *px = packed;
        }
    }

    /// Vertical line from `y0` to `y1` (inclusive) at column `col`, clipped.
    pub fn vline(&mut self, y0: i32, y1: i32, col: i32, color: Color) {
        if col < 0 || col >= self.width {
            return;
        }
        let (mut top, mut bottom) = (y0.min(y1), y0.max(y1));
        top = top.max(0);
        bottom = bottom.min(self.height - 1);
        if top > bottom {
            return;
        }
        let packed = color.to_xrgb8888();
        // `top`/`bottom` are clamped non-negative above, `self.width` is
        // always non-negative (checked in Canvas::new), and `col` is
        // checked in-bounds above, so these sign-preserving casts are exact.
        let stride = self.width.cast_unsigned() as usize;
        for y in top..=bottom {
            self.pixels[(y.cast_unsigned() as usize) * stride + col.cast_unsigned() as usize] =
                packed;
        }
    }

    /// Blit a decoded RGBA8 image (`src`, `src_w`×`src_h` pixels, straight
    /// alpha, row-major) into `dest` (clipped to the canvas), nearest-
    /// neighbor scaled to fit, and alpha-composited exactly like
    /// [`Self::fill_rect_alpha`]/[`text::blit_glyph`]'s "solid ink over
    /// glass" contract: each source pixel's own alpha blends it against
    /// `bg`, then the *result* is treated as opaque ink -- i.e. `bg_alpha`
    /// (the window's own translucency) does not further fade the icon,
    /// same as glyph ink never fading with the window. This is app-icon
    /// rendering's one blit primitive (see `bins/launcher`'s icon module);
    /// nearest-neighbor is deliberate over any smoother filter -- icons are
    /// blitted at roughly their native size (XDG icon themes ship several
    /// discrete sizes precisely so callers don't need to resample much),
    /// so the softness a bilinear filter buys isn't worth the extra math
    /// for a handful of ~24px launcher rows per frame.
    pub fn blit_rgba_scaled(
        &mut self,
        dest: Rect,
        src: &[u8],
        src_w: i32,
        src_h: i32,
        bg: Color,
        bg_alpha: u8,
    ) {
        if src_w <= 0 || src_h <= 0 || dest.w <= 0 || dest.h <= 0 {
            return;
        }
        let Some((x0, y0, x1, y1)) = self.clip(dest) else {
            return;
        };
        let stride = self.width.cast_unsigned() as usize;
        for y in y0..y1 {
            // Nearest-neighbor source row for this destination row.
            let src_y = (y - dest.y) * src_h / dest.h;
            let src_y = src_y.clamp(0, src_h - 1).cast_unsigned() as usize;
            for x in x0..x1 {
                let src_x = (x - dest.x) * src_w / dest.w;
                let src_x = src_x.clamp(0, src_w - 1).cast_unsigned() as usize;
                let idx = (src_y * src_w.cast_unsigned() as usize + src_x) * 4;
                let Some(px) = src.get(idx..idx + 4) else {
                    continue;
                };
                let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
                if a == 0 {
                    continue;
                }
                let ink = Color::rgb(r, g, b).blend_opaque_color(bg, 255 - a);
                let packed = if bg_alpha == 255 {
                    ink.to_xrgb8888()
                } else {
                    ink.premultiplied_argb8888(255)
                };
                let out_idx = (y.cast_unsigned() as usize) * stride + x.cast_unsigned() as usize;
                self.pixels[out_idx] = packed;
            }
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
        vec![0u32; usize::try_from(w * h).unwrap_or(0)]
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
    fn fill_rect_alpha_opaque_matches_fill_rect() {
        // bg_alpha == 255 must be byte-identical to the plain fill_rect
        // path -- both should pack the same zero-top-byte XRGB8888 word.
        let mut buf_a = canvas_buf(4, 4);
        let mut buf_b = canvas_buf(4, 4);
        Canvas::new(&mut buf_a, 4, 4).fill_rect(Rect::new(1, 1, 2, 2), Color::WHITE);
        Canvas::new(&mut buf_b, 4, 4).fill_rect_alpha(Rect::new(1, 1, 2, 2), Color::WHITE, 255);
        assert_eq!(buf_a, buf_b);
    }

    #[test]
    fn fill_rect_alpha_translucent_packs_premultiplied_argb_not_zero_alpha() {
        // On a translucent window (bg_alpha < 255) the destination buffer is
        // Argb8888, where the top byte is the alpha the compositor reads.
        // fill_rect's plain to_xrgb8888() packs a zero top byte there,
        // which would make the fill invisible; fill_rect_alpha must pack a
        // fully-opaque (0xff) top byte instead ("solid ink" over glass),
        // regardless of the window's own bg_alpha.
        let mut buf = canvas_buf(2, 1);
        Canvas::new(&mut buf, 2, 1).fill_rect_alpha(Rect::new(0, 0, 2, 1), Color::WHITE, 128);
        // premultiplied_argb8888(255) on white is 0xff_ff_ff_ff.
        assert_eq!(buf[0], 0xff_ff_ff_ff);
        assert_eq!(buf[1], 0xff_ff_ff_ff);
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
    fn blend_opaque_div255_matches_exact_division_for_every_input() {
        // blend_opaque_to swapped `/255` for a multiply-shift div255. Prove it
        // is bit-identical to real division across every (channel, alpha) pair,
        // so glyph colors can never drift by a least-significant bit.
        for from in [0u8, 1, 63, 127, 128, 200, 254, 255] {
            for to in [0u8, 1, 63, 127, 128, 200, 254, 255] {
                for a in 0u32..=255 {
                    let num = u32::from(from) * (255 - a) + u32::from(to) * a;
                    let packed = Color::rgb(from, from, from)
                        .blend_opaque_to(Color::rgb(to, to, to), u8::try_from(a).unwrap_or(255));
                    let ch = packed & 0xff;
                    assert_eq!(ch, num / 255, "from={from} to={to} a={a}");
                }
            }
        }
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
                let px = buf[row * usize::try_from(font::GLYPH_W).unwrap_or(0)
                    + usize::try_from(col).unwrap_or(0)];
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
    fn blit_rgba_scaled_nearest_neighbor_upscales() {
        // A 1x1 opaque-red source, blitted into a 2x2 destination, should
        // fill every destination pixel with red (nearest-neighbor of a
        // single source pixel is itself, everywhere).
        let mut buf = canvas_buf(2, 2);
        let mut c = Canvas::new(&mut buf, 2, 2);
        let src = [255u8, 0, 0, 255]; // opaque red
        c.blit_rgba_scaled(Rect::new(0, 0, 2, 2), &src, 1, 1, Color::BLACK, 255);
        let red = Color::rgb(255, 0, 0).to_xrgb8888();
        assert_eq!(buf, vec![red, red, red, red]);
    }

    #[test]
    fn blit_rgba_scaled_skips_fully_transparent_pixels() {
        // A fully transparent source pixel leaves the destination untouched
        // rather than painting black -- important for icons with rounded
        // corners/padding, where the alpha=0 border shouldn't punch a
        // visible hole in whatever's already drawn underneath.
        let mut buf = canvas_buf(1, 1);
        buf[0] = Color::WHITE.to_xrgb8888();
        let mut c = Canvas::new(&mut buf, 1, 1);
        let src = [0u8, 0, 0, 0]; // transparent black
        c.blit_rgba_scaled(Rect::new(0, 0, 1, 1), &src, 1, 1, Color::BLACK, 255);
        assert_eq!(buf[0], Color::WHITE.to_xrgb8888());
    }

    #[test]
    fn blit_rgba_scaled_composites_partial_alpha_over_bg() {
        // A half-alpha source pixel should blend toward `bg`, not just copy
        // its own RGB through untouched.
        let mut buf = canvas_buf(1, 1);
        let mut c = Canvas::new(&mut buf, 1, 1);
        let src = [255u8, 255, 255, 128]; // half-alpha white
        c.blit_rgba_scaled(Rect::new(0, 0, 1, 1), &src, 1, 1, Color::BLACK, 255);
        // Should be roughly mid-gray (white blended 50% over black), not
        // pure white and not pure black.
        let packed = buf[0];
        let r = (packed >> 16) & 0xff;
        assert!(
            r > 0x30 && r < 0xd0,
            "expected mid-gray blend, got {packed:#x}"
        );
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
