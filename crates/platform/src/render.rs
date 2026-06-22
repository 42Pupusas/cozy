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

/// Terminal default fg/bg (matches a dark theme).
const DEFAULT_FG: DrawColor = DrawColor::rgb(0xcc, 0xcc, 0xcc);
const DEFAULT_BG: DrawColor = DrawColor::rgb(0x1e, 0x1e, 0x2e);

/// Default background as a packed XRGB8888 word — used to clear the backbuffer.
pub const DEFAULT_BG_XRGB: u32 = DEFAULT_BG.to_xrgb8888();

fn resolve(color: Color, default: DrawColor) -> DrawColor {
    match color {
        Color::Default => default,
        Color::Indexed(n) => {
            let (r, g, b) = PALETTE[n as usize];
            DrawColor::rgb(r, g, b)
        }
        Color::Rgb(r, g, b) => DrawColor::rgb(r, g, b),
    }
}

fn effective(attrs: &Attrs) -> (DrawColor, DrawColor) {
    let fg = resolve(attrs.fg, DEFAULT_FG);
    let bg = resolve(attrs.bg, DEFAULT_BG);
    if attrs.inverse {
        (bg, fg)
    } else {
        (fg, bg)
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
) {
    let (fg, bg) = effective(&cell.attrs);
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
pub fn blit_snapshot(pixels: &mut [u32], buf_width: u32, snap: &DirtySnapshot) {
    for row in &snap.rows {
        for (col, cell) in row.cells.iter().enumerate() {
            blit_cell(pixels, buf_width, col as u16, row.row, cell);
        }
    }
}
