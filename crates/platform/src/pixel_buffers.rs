#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! The window's persistent backbuffer.
//!
//! Extracted from [`crate::window::TermWindow`] because this is genuinely
//! self-contained pixel math with no Wayland, grid, or selection coupling.
//! `width`/`height` themselves stay on `TermWindow` (they're threaded
//! through cell-math, pointer-hit-testing, and Wayland configure handling
//! far beyond pixel storage), so every method here takes them as parameters
//! instead of owning a second copy that could drift out of sync.

/// Persistent full-window backbuffer, already packed for the surface's
/// buffer format.
pub struct PixelBuffers {
    /// Dirty rows are blitted here (it keeps the complete last-known image),
    /// then copied into whichever shm frame is free on present. Without
    /// this, incremental dirty-row blits would only ever touch one of the
    /// two shm frames, leaving the rest black.
    backbuffer: Vec<u32>,
}

impl PixelBuffers {
    /// Allocate the buffer at `width x height`, filled with `bg` (already
    /// packed per [`crate::render::Renderer::bg_packed`] -- callers must use
    /// `bg_packed`, not `bg_xrgb`, so a translucent window's initial
    /// backbuffer carries the configured alpha from frame one instead of
    /// flashing fully opaque until the first real paint).
    #[must_use]
    pub fn new(width: i32, height: i32, bg: u32) -> Self {
        Self {
            backbuffer: vec![bg; (width * height).max(0) as usize],
        }
    }

    /// Read-only view of the backbuffer.
    #[must_use]
    pub fn backbuffer(&self) -> &[u32] {
        &self.backbuffer
    }

    /// Mutable view of the backbuffer, for direct pixel writes (overlay
    /// tinting, cursor/underline drawing) that don't go through a
    /// [`crate::draw::Canvas`].
    pub fn backbuffer_mut(&mut self) -> &mut [u32] {
        &mut self.backbuffer
    }

    /// A [`crate::draw::Canvas`] borrowing the backbuffer at `width x height`
    /// for the caller to blit/paint into.
    pub fn canvas(&mut self, width: i32, height: i32) -> crate::draw::Canvas<'_> {
        crate::draw::Canvas::new(&mut self.backbuffer, width, height)
    }

    /// Resize the buffer to `new_w x new_h`, carrying the old image into the
    /// new one (top-left aligned) instead of clearing to background. Used on a
    /// window resize: the grid reflow + full snapshot arrives a round-trip
    /// later via the parser thread, so keeping the previous content visible
    /// avoids a blank flash mid-drag. `bg` must be `bg_packed()` (not
    /// `bg_xrgb()`) so newly-exposed area on a grow carries the configured
    /// window alpha instead of flashing opaque.
    pub fn resize(&mut self, old_w: i32, old_h: i32, new_w: i32, new_h: i32, bg: u32) {
        let (new_w, new_h) = (new_w.max(0) as usize, new_h.max(0) as usize);
        let (old_w, old_h) = (old_w.max(0) as usize, old_h.max(0) as usize);
        let mut new_back = vec![bg; new_w * new_h];
        let copy_w = old_w.min(new_w);
        let copy_h = old_h.min(new_h);
        for row in 0..copy_h {
            let src = &self.backbuffer[row * old_w..row * old_w + copy_w];
            new_back[row * new_w..row * new_w + copy_w].copy_from_slice(src);
        }
        self.backbuffer = new_back;
    }

    /// Clear the scanlines below the last complete terminal cell row.
    ///
    /// Wayland configures are pixel-sized, while the grid deliberately uses
    /// only complete cells. During a resize [`Self::resize`] preserves the
    /// old image until the parser supplies a reflowed snapshot; without this
    /// cleanup, that preservation can copy the top of an old final glyph into
    /// the new size's fractional bottom strip. No grid row owns those pixels,
    /// so a normal dirty-row paint can never overwrite them and the fragment
    /// remains visible indefinitely. The strip is presentation-only space and
    /// must always be plain terminal background.
    pub fn clear_partial_cell_row(&mut self, width: i32, height: i32, cell_h: i32, bg: u32) {
        let (width, height, cell_h) = (width.max(0) as usize, height.max(0) as usize, cell_h);
        if width == 0 || cell_h <= 0 {
            return;
        }
        let complete_height = (height / cell_h as usize) * cell_h as usize;
        if complete_height == height {
            return;
        }
        let start = complete_height * width;
        if let Some(strip) = self.backbuffer.get_mut(start..) {
            strip.fill(bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_backbuffer() {
        let buf = PixelBuffers::new(2, 2, 0x00_11_22_33);
        assert_eq!(buf.backbuffer(), &[0x00_11_22_33; 4]);
    }

    #[test]
    fn resize_grow_preserves_old_content_top_left_aligned() {
        let mut buf = PixelBuffers::new(2, 2, 0x00_00_00_00);
        buf.backbuffer_mut().copy_from_slice(&[1, 2, 3, 4]);
        buf.resize(2, 2, 3, 3, 0x00_ff_ff_ff);
        // Row 0: old row 0 (1,2) then fill; row 1: old row 1 (3,4) then fill;
        // row 2: all fill.
        assert_eq!(
            buf.backbuffer(),
            &[
                1,
                2,
                0x00_ff_ff_ff,
                3,
                4,
                0x00_ff_ff_ff,
                0x00_ff_ff_ff,
                0x00_ff_ff_ff,
                0x00_ff_ff_ff
            ]
        );
    }

    #[test]
    fn resize_shrink_crops_to_the_new_dimensions() {
        let mut buf = PixelBuffers::new(3, 3, 0);
        buf.backbuffer_mut()
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        buf.resize(3, 3, 2, 2, 0);
        // Top-left 2x2 of the old 3x3: rows [1,2] and [4,5].
        assert_eq!(buf.backbuffer(), &[1, 2, 4, 5]);
    }

    #[test]
    fn clear_partial_cell_row_removes_preserved_glyph_fragment() {
        // Mimic an old image copied across a pixel resize: the bottom two
        // scanlines are not a complete 4px cell and must not retain glyph ink.
        let mut buf = PixelBuffers::new(2, 6, 0);
        buf.backbuffer_mut().fill(0x00_ff_00_ff);
        buf.clear_partial_cell_row(2, 6, 4, 0x00_11_22_33);
        assert_eq!(
            buf.backbuffer(),
            &[
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_ff_00_ff,
                0x00_11_22_33,
                0x00_11_22_33,
                0x00_11_22_33,
                0x00_11_22_33,
            ]
        );
    }
}
