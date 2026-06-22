#![allow(clippy::cast_possible_truncation)]
//! Software terminal renderer: blit grid cells into a `u32` pixel buffer.
//!
//! Uses the embedded 8×16 bitmap font from [`crate::font`]. Each terminal cell
//! occupies `GLYPH_W × GLYPH_H` pixels. For now only the 95 printable-ASCII
//! glyphs are rendered; non-ASCII falls back to a solid block.
//!
//! Color mapping: converts 256-palette indices to 24-bit RGB using the standard
//! xterm color table; true-color cells pass through directly.

use grid::{Attrs, Cell, Color, DirtySnapshot};

use crate::draw::Color as DrawColor;
use crate::font::{FIRST, GLYPH_H, GLYPH_W, LAST};

pub use crate::font::{FONT, GLYPH_H as CELL_H, GLYPH_W as CELL_W};

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
        Self { ansi, fg: DEFAULT_FG, bg: DEFAULT_BG }
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

/// Blit a single cell into the pixel buffer.
///
/// `pixels` is the full framebuffer slice (`width × height` u32 words),
/// `buf_width` is the stride in pixels, `cx`/`cy` are the cell column/row.
pub fn blit_cell(
    pixels: &mut [u32],
    buf_width: u32,
    cx: u16,
    cy: u16,
    cell: &Cell,
    theme: &Theme,
) {
    let (fg, bg) = theme.effective(&cell.attrs);
    let fg_px = fg.to_xrgb8888();
    let bg_px = bg.to_xrgb8888();

    let px0 = u32::from(cx) * GLYPH_W as u32;
    let py0 = u32::from(cy) * GLYPH_H as u32;

    // Look up glyph bitmap.
    let ch = if cell.attrs.invisible { ' ' } else { cell.ch };
    let glyph: Option<&[u8; 16]> = if ch >= FIRST as char && ch <= LAST as char {
        let idx = ch as usize - FIRST as usize;
        FONT.get(idx)
    } else {
        None
    };

    for row in 0..GLYPH_H as u32 {
        let py = py0 + row;
        let row_mask: u8 = glyph.map_or(0xff, |g| g[row as usize]);
        for col in 0..GLYPH_W as u32 {
            let px = px0 + col;
            let bit = (row_mask >> (7 - col)) & 1;
            let pixel = if bit != 0 { fg_px } else { bg_px };
            let idx = (py * buf_width + px) as usize;
            if idx < pixels.len() {
                pixels[idx] = pixel;
            }
        }
    }
}

/// Blit every dirty row in `snap` into the framebuffer.
///
/// `pixels` is the entire framebuffer (`buf_width × buf_height` u32 words).
pub fn blit_snapshot(pixels: &mut [u32], buf_width: u32, snap: &DirtySnapshot, theme: &Theme) {
    for row in &snap.rows {
        for (col, cell) in row.cells.iter().enumerate() {
            blit_cell(pixels, buf_width, col as u16, row.row, cell, theme);
        }
    }
}
