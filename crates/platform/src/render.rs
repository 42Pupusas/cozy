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
    // Spacer cells are the trailing (invisible) half of a wide glyph: the
    // lead cell paints both columns already, so drawing here would just
    // stamp a redundant blank over it (harmless but wasteful) -- skip.
    if cell.spacer {
        return;
    }

    let (fg, bg) = theme.effective(&cell.attrs);
    let fg_px = fg.to_xrgb8888();
    let bg_px = bg.to_xrgb8888();

    let px0 = u32::from(cx) * GLYPH_W as u32;
    let py0 = u32::from(cy) * GLYPH_H as u32;
    // A wide cell occupies two glyph cells' worth of background so the fill
    // doesn't leave a bg-colored gap under its spacer half.
    let cell_cols: u32 = if cell.wide { 2 } else { 1 };

    // Look up glyph bitmap.
    let ch = if cell.attrs.invisible { ' ' } else { cell.ch };
    let glyph: Option<&[u8; 16]> = if ch >= FIRST as char && ch <= LAST as char {
        let idx = ch as usize - FIRST as usize;
        FONT.get(idx)
    } else {
        None
    };

    // Unknown glyphs (no ASCII bitmap) fall back to a solid ink block so
    // missing coverage is visible rather than silently blank -- except for
    // a wide cell's synthetic second half, which stays plain background.
    for row in 0..GLYPH_H as u32 {
        let py = py0 + row;
        let row_mask: u8 = glyph.map_or(0xff, |g| g[row as usize]);
        for col in 0..(GLYPH_W as u32 * cell_cols) {
            let px = px0 + col;
            // Clip to the buffer's actual bounds. The `px < buf_width` guard is
            // critical: without it a cell from a STALE wider-geometry snapshot
            // (one produced before a shrink, still queued when the backbuffer
            // was already rebuilt narrower) would have px >= buf_width and the
            // flat `py*buf_width + px` index would silently wrap into the NEXT
            // row -- reordering and swallowing lines mid-drag. A plain
            // `idx < len` check alone does not catch that horizontal overflow.
            if px >= buf_width {
                continue;
            }
            // Only the first GLYPH_W columns carry the (ASCII) bitmap; a wide
            // cell's second half is background-only until a real font with
            // CJK/emoji glyphs replaces the embedded bitmap.
            let bit = if col < GLYPH_W as u32 { (row_mask >> (7 - col)) & 1 } else { 0 };
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
    // Rows of the glyph grid that fit in this buffer. A stale snapshot from a
    // taller geometry may carry rows past the bottom; clip them rather than let
    // them paint into the wrong pixels.
    let buf_rows = (pixels.len() as u32 / buf_width.max(1)) / GLYPH_H as u32;
    for row in &snap.rows {
        if u32::from(row.row) >= buf_rows {
            continue;
        }
        for (col, cell) in row.cells.iter().enumerate() {
            blit_cell(pixels, buf_width, col as u16, row.row, cell, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grid::{DirtyRow, DirtySnapshot};

    fn cell(ch: char) -> Cell {
        Cell { ch, attrs: Attrs::default(), wide: false, spacer: false }
    }

    #[test]
    fn stale_wide_snapshot_does_not_bleed_into_next_row() {
        // Backbuffer sized for a NARROW grid: 2 cells wide, 2 rows tall.
        let bw = 2 * CELL_W as u32;
        let bh = 2 * CELL_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];

        // A STALE snapshot from a wider geometry: row 0 has 5 cells. Without
        // the px < buf_width clip, cells 2..5 would wrap into row 0's lower
        // pixel lines / row 1. We assert row 1's cell region stays untouched.
        let snap = DirtySnapshot {
            cols: 5,
            rows: vec![DirtyRow { row: 0, cells: (0..5).map(|_| cell('M')).collect() }],
        };
        let theme = Theme::builtin();
        blit_snapshot(&mut pixels, bw, &snap, &theme);

        // The second glyph row (pixels y in [CELL_H, 2*CELL_H)) must be all
        // background — nothing from the over-wide row 0 may have leaked there.
        let bg = theme.bg.to_xrgb8888();
        for y in CELL_H as u32..bh {
            for x in 0..bw {
                let px = pixels[(y * bw + x) as usize];
                assert_eq!(px, 0, "row 1 pixel ({x},{y}) was painted: {px:#x} (bg={bg:#x})");
            }
        }
    }

    #[test]
    fn stale_tall_snapshot_row_is_clipped() {
        // Backbuffer for 2 rows; a stale snapshot references row 3.
        let bw = 2 * CELL_W as u32;
        let bh = 2 * CELL_H as u32;
        let mut pixels = vec![0u32; (bw * bh) as usize];
        let snap = DirtySnapshot {
            cols: 2,
            rows: vec![DirtyRow { row: 3, cells: vec![cell('M'), cell('M')] }],
        };
        blit_snapshot(&mut pixels, bw, &snap, &Theme::builtin());
        // Nothing should have been drawn at all.
        assert!(pixels.iter().all(|&p| p == 0), "off-bottom row 3 leaked into buffer");
    }
}
