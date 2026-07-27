#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! The window's pixel-level backing stores: the persistent XRGB backbuffer
//! and its premultiplied-ARGB twin for custom (translucent) painters.
//!
//! Extracted from [`crate::window::TermWindow`] because this is genuinely
//! self-contained pixel math with no Wayland, grid, or selection coupling --
//! unlike the frame-presentation/overlay-compositing pipeline (which reaches
//! into selection/cursor/renderer state and stays on `TermWindow`), this
//! struct only ever needs a width/height and a background/alpha to do its
//! job. `width`/`height` themselves stay on `TermWindow` (they're threaded
//! through cell-math, pointer-hit-testing, and Wayland configure handling
//! far beyond pixel storage), so every method here takes them as parameters
//! instead of owning a second copy that could drift out of sync.

/// Persistent full-window backbuffer plus the cached premultiplied-ARGB copy
/// custom (translucent) painters read on present, and the SIMD dispatch that
/// rebuilds the latter from the former.
pub struct PixelBuffers {
    /// Dirty rows are blitted here (it keeps the complete last-known image),
    /// then copied into whichever shm frame is free on present. Without
    /// this, incremental dirty-row blits would only ever touch one of the
    /// two shm frames, leaving the rest black.
    backbuffer: Vec<u32>,
    /// Cached premultiplied pixels for custom painters. Kept separate from
    /// the stable XRGB backbuffer avoids re-premultiplying every stale row
    /// on every shm-slot upload.
    custom_argb: Vec<u32>,
    /// SIMD dispatch for the premultiply loop, probed once at construction
    /// (see [`crate::premultiply::Premultiplier`]) instead of per present --
    /// feature detection is a cached atomic load, but even that cost has no
    /// reason to repeat every frame.
    premultiplier: crate::premultiply::Premultiplier,
}

impl PixelBuffers {
    /// Allocate both buffers at `width x height`, filled with `bg` (already
    /// packed per [`crate::render::Renderer::bg_packed`] -- callers must use
    /// `bg_packed`, not `bg_xrgb`, so a translucent window's initial
    /// backbuffer carries the configured alpha from frame one instead of
    /// flashing fully opaque until the first real paint), and seed
    /// `custom_argb` by premultiplying that fill at `alpha`.
    #[must_use]
    pub fn new(width: i32, height: i32, bg: u32, alpha: u8) -> Self {
        let backbuffer = vec![bg; (width * height).max(0) as usize];
        let premultiplier = crate::premultiply::Premultiplier::new();
        let mut custom_argb = vec![0u32; backbuffer.len()];
        premultiplier.run(alpha, &backbuffer, &mut custom_argb);
        Self {
            backbuffer,
            custom_argb,
            premultiplier,
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

    /// Read-only view of the premultiplied ARGB cache.
    #[must_use]
    pub fn custom_argb(&self) -> &[u32] {
        &self.custom_argb
    }

    /// A [`crate::draw::Canvas`] borrowing the backbuffer at `width x height`
    /// for the caller to blit/paint into.
    pub fn canvas(&mut self, width: i32, height: i32) -> crate::draw::Canvas<'_> {
        crate::draw::Canvas::new(&mut self.backbuffer, width, height)
    }

    /// Premultiplies one packed `0x00RRGGBB` pixel into `0xAARRGGBB` at
    /// `alpha`, using the same fixed-point divide-by-255 identity the SIMD
    /// kernel uses (see [`crate::premultiply::Premultiplier`]) -- for the
    /// rare single-pixel case ([`Self::copy_stale_rows_source`]'s custom
    /// background fill) where spinning up the vector kernel for one pixel
    /// isn't worth it.
    #[must_use]
    pub fn premultiply_pixel(pixel: u32, alpha: u8) -> u32 {
        let a = u32::from(alpha);
        let rgb = pixel & 0x00ff_ffff;
        let r = ((rgb >> 16) & 0xff) * a / 255;
        let g = ((rgb >> 8) & 0xff) * a / 255;
        let b = (rgb & 0xff) * a / 255;
        (a << 24) | (r << 16) | (g << 8) | b
    }

    /// Rebuild the damaged rows of `custom_argb` from the backbuffer at
    /// `alpha`, for a `width`-wide buffer. No-op (cheaply -- one branch) when
    /// `needs_alpha` is false, since an opaque custom painter never reads
    /// `custom_argb` at all.
    ///
    /// Profiling a live translucent session found the naive per-pixel
    /// `channel * alpha / 255` scalar loop alone costing ~18ms/frame,
    /// dwarfing the actual dashboard paint (~0.2ms) -- this dispatches to
    /// [`crate::premultiply::Premultiplier`]'s SIMD kernel (AVX2/SSE2 on
    /// `x86_64`, scalar elsewhere) instead.
    pub fn update_custom_argb(&mut self, width: i32, damage: crate::draw::Rect, alpha: u8) {
        let width = width.max(0) as usize;
        if width == 0 {
            return;
        }
        // Configure dimensions and the allocated backbuffer size can differ
        // by a partial final row. Clamp against the actual caches before
        // forming slices; custom damage must never turn a compositor resize
        // race into an out-of-bounds access.
        let rows = (self.backbuffer.len() / width).min(self.custom_argb.len() / width);
        let y0 = damage.y.max(0).min(rows as i32) as usize;
        let y1 = damage.bottom().max(y0 as i32).min(rows as i32) as usize;
        // One call over the whole damaged band rather than per-row: the rows
        // are contiguous in both buffers, so this is a single premultiply
        // pass instead of `rows` separate short ones, keeping the vector
        // kernels' loop overhead amortized across the entire span.
        let start = y0 * width;
        let end = y1 * width;
        self.premultiplier.run(
            alpha,
            &self.backbuffer[start..end],
            &mut self.custom_argb[start..end],
        );
    }

    /// Resize both buffers to `new_w x new_h`, carrying the old backbuffer's
    /// image into the new one (top-left aligned) instead of clearing to
    /// background, and rebuilding `custom_argb` at the new size. Used on a
    /// window resize: the grid reflow + full snapshot arrives a round-trip
    /// later via the parser thread, so keeping the previous content visible
    /// avoids a blank flash mid-drag. `bg` must be `bg_packed()` (not
    /// `bg_xrgb()`) so newly-exposed area on a grow carries the configured
    /// window alpha instead of flashing opaque.
    pub fn resize(&mut self, old_w: i32, old_h: i32, new_w: i32, new_h: i32, bg: u32, alpha: u8) {
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
        // Rebuild the premultiplied custom-painter cache to the same
        // physical dimensions as the resized backbuffer. Keeping the old
        // cache here leaves the final rows stale and causes the bottom edge
        // to oscillate while the compositor alternates shm slots.
        let mut custom_argb = vec![0u32; self.backbuffer.len()];
        self.premultiplier
            .run(alpha, &self.backbuffer, &mut custom_argb);
        self.custom_argb = custom_argb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_fills_backbuffer_and_seeds_custom_argb() {
        let buf = PixelBuffers::new(2, 2, 0x00_11_22_33, 255);
        assert_eq!(buf.backbuffer(), &[0x00_11_22_33; 4]);
        // alpha=255 premultiply is an identity on RGB with 0xff in the top byte.
        assert_eq!(buf.custom_argb(), &[0xff_11_22_33; 4]);
    }

    #[test]
    fn premultiply_pixel_zero_alpha_is_all_zero() {
        assert_eq!(PixelBuffers::premultiply_pixel(0x00_ff_ff_ff, 0), 0);
    }

    #[test]
    fn premultiply_pixel_full_alpha_is_identity_on_rgb() {
        assert_eq!(
            PixelBuffers::premultiply_pixel(0x00_10_20_30, 255),
            0xff_10_20_30
        );
    }

    #[test]
    fn resize_grow_preserves_old_content_top_left_aligned() {
        let mut buf = PixelBuffers::new(2, 2, 0x00_00_00_00, 255);
        buf.backbuffer_mut().copy_from_slice(&[1, 2, 3, 4]);
        buf.resize(2, 2, 3, 3, 0x00_ff_ff_ff, 255);
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
        let mut buf = PixelBuffers::new(3, 3, 0, 255);
        buf.backbuffer_mut()
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
        buf.resize(3, 3, 2, 2, 0, 255);
        // Top-left 2x2 of the old 3x3: rows [1,2] and [4,5].
        assert_eq!(buf.backbuffer(), &[1, 2, 4, 5]);
    }

    #[test]
    fn update_custom_argb_only_touches_the_damaged_rows() {
        let mut buf = PixelBuffers::new(2, 3, 0x00_10_20_30, 0);
        // custom_argb starts all-zero (alpha=0 premultiply). Directly poke
        // the backbuffer with a distinct color, then damage only row 1.
        buf.backbuffer_mut().copy_from_slice(&[
            0x00_a0_a0_a0,
            0x00_a0_a0_a0,
            0x00_b0_b0_b0,
            0x00_b0_b0_b0,
            0x00_c0_c0_c0,
            0x00_c0_c0_c0,
        ]);
        buf.update_custom_argb(2, crate::draw::Rect::new(0, 1, 2, 1), 255);
        let argb = buf.custom_argb();
        // Row 0 and row 2 untouched (still zero from construction).
        assert_eq!(argb[0], 0);
        assert_eq!(argb[4], 0);
        // Row 1 rebuilt at full alpha.
        assert_eq!(argb[2], 0xff_b0_b0_b0);
        assert_eq!(argb[3], 0xff_b0_b0_b0);
    }
}
