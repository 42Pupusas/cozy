//! `StatusNotifier` tray-icon pixels and their software-canvas renderer.
//!
//! The `StatusNotifierItem` protocol exposes `IconPixmap` as one or more raw
//! ARGB32 pixmaps. Keeping their validation, size selection, and painting in
//! this module means the D-Bus watcher only has to obtain typed icon data; it
//! never needs to know about the bar's framebuffer or Wayland scaling.

use platform::draw::Canvas;

/// One `IconPixmap` entry supplied by a `StatusNotifierItem`.
///
/// The protocol stores pixels as 32-bit ARGB values in row-major order. The
/// vector is deliberately kept in wire order: `Canvas` uses the same packed
/// layout for its opaque XRGB path, and its ARGB path accepts the same channel
/// order after the icon's alpha has been composited by [`TrayIcon::paint`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayPixmap {
    width: i32,
    height: i32,
    argb: Vec<u32>,
}

impl TrayPixmap {
    /// Build a validated pixmap. Invalid dimensions or a mismatched pixel
    /// count are rejected instead of letting a misbehaving tray client cause
    /// an out-of-bounds read during bar painting.
    #[must_use]
    pub fn new(width: i32, height: i32, argb: Vec<u32>) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let pixels = usize::try_from(width.checked_mul(height)?).ok()?;
        (argb.len() == pixels).then_some(Self {
            width,
            height,
            argb,
        })
    }

    /// Pixel width advertised by the tray item.
    #[must_use]
    pub const fn width(&self) -> i32 {
        self.width
    }

    /// Pixel height advertised by the tray item.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Return the pixel at `(x, y)`. Coordinates are guaranteed in range by
    /// callers inside this module, so this remains a private indexing detail.
    fn pixel_at(&self, x: i32, y: i32) -> u32 {
        let index = usize::try_from(y * self.width + x).unwrap_or(0);
        self.argb[index]
    }

    /// Clamp-to-edge pixel fetch: a sample position just past the last
    /// row/column (the `+1` neighbor in area-averaged downscaling) must not
    /// read out of bounds or wrap.
    fn pixel_at_clamped(&self, x: i32, y: i32) -> u32 {
        self.pixel_at(x.clamp(0, self.width - 1), y.clamp(0, self.height - 1))
    }
}

/// An item's normal or attention icon, potentially with several resolution alternatives.
///
/// The renderer chooses the smallest icon that can satisfy the requested
/// square, avoiding needless downsampling of a huge pixmap while still
/// preferring sharp enough artwork on `HiDPI` outputs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrayIcon {
    pixmaps: Vec<TrayPixmap>,
}

impl TrayIcon {
    /// Make an icon from the protocol's pixmap alternatives.
    #[must_use]
    pub const fn new(pixmaps: Vec<TrayPixmap>) -> Self {
        Self { pixmaps }
    }

    /// Whether this icon contains no paintable pixmap.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pixmaps.is_empty()
    }

    /// Paint the best available pixmap centered within a `size × size`
    /// square at `(x,y)`. Drawn at native resolution whenever the source
    /// already fits the slot (no upscaling -- synthesizing pixels that were
    /// never there just looks soft next to the bar's crisp vector glyph
    /// text). But when the source is *larger* than the slot it is properly
    /// downscaled to fit, using area averaging rather than a naive
    /// nearest-neighbor drop-sample -- point-sampling a shrink discards most
    /// of the source's pixels outright and looks jagged/aliased, while
    /// averaging every source pixel that falls under each output pixel's
    /// footprint is the textbook-correct way to shrink and reads smooth.
    /// `best_for` already tries to pick a pixmap close to the requested
    /// size, so this path triggers only when a client offers nothing small
    /// enough (Mattermost: a single 32x32 pixmap on a non-HiDPI, ~20px slot).
    ///
    /// `StatusNotifier` pixmaps have straight ARGB alpha. The bar can itself be
    /// translucent, but tray artwork must remain opaque ink after it is
    /// blended over the already-cleared bar background; composing
    /// source-over here avoids black boxes around semi-transparent
    /// application logos.
    ///
    /// `bg_alpha` (`0-255`, `255` = opaque) is the bar's own window opacity
    /// -- see `platform::draw::Color::blend_glyph`'s identical parameter.
    /// On an opaque bar the backbuffer is packed `Xrgb8888` and every pixel
    /// this paints must leave the destination's top byte at `0` (that byte
    /// is simply ignored by the compositor, so any value is technically
    /// harmless there, but `0` keeps the buffer's bytes meaningful if it's
    /// ever read back). On a translucent bar the backbuffer is
    /// **premultiplied** `Argb8888` per `wl_shm`'s wire format (see
    /// `platform::surface::SurfaceBootstrap::pack_bg`): every partially
    /// covered icon pixel (antialiased edges, downscaled coverage) must
    /// composite `ink-alpha` over `dst-alpha` in premultiplied space and
    /// write a real, non-zero alpha byte -- forcing that byte to `0` while
    /// leaving non-zero RGB behind (the previous bug) hands the compositor
    /// bytes that violate the premultiplied invariant `rgb <= alpha`,
    /// which is exactly the kind of corrupted/distorted edge fringing this
    /// was reported as.
    pub fn paint(&self, canvas: &mut Canvas<'_>, x: i32, y: i32, size: i32, bg_alpha: u8) {
        let Some(source) = self.best_for(size) else {
            return;
        };
        let slot = size.max(1);
        let native_width = source.width.max(1);
        let native_height = source.height.max(1);
        // Never upscale: only shrink to fit when the source overflows the
        // slot on either axis, preserving aspect ratio.
        let downscale = f64::from(slot) / f64::from(native_width.max(native_height));
        let scale = downscale.min(1.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let draw_width = ((f64::from(native_width) * scale).round() as i32).max(1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let draw_height = ((f64::from(native_height) * scale).round() as i32).max(1);

        let offset_x = x + (slot - draw_width) / 2;
        let offset_y = y + (slot - draw_height) / 2;
        // Read geometry before taking the canvas's mutable pixel borrow.
        let canvas_width = canvas.width();
        let canvas_height = canvas.height();
        let pixels = canvas.pixels_mut();

        for dy in 0..draw_height {
            let py = offset_y + dy;
            if py < 0 || py >= canvas_height {
                continue;
            }
            for dx in 0..draw_width {
                let px = offset_x + dx;
                if px < 0 || px >= canvas_width {
                    continue;
                }
                let src = if draw_width == native_width && draw_height == native_height {
                    source.pixel_at(dx, dy)
                } else {
                    Self::area_average_pixel(source, dx, dy, draw_width, draw_height)
                };
                let alpha = (src >> 24) as u8;
                if alpha == 0 {
                    continue;
                }
                let index = usize::try_from(py * canvas_width + px).unwrap_or(0);
                let dst = pixels[index];
                pixels[index] = Self::over(src, dst, alpha, bg_alpha);
            }
        }
    }

    /// Downscale-only resampling: box-filter every source pixel whose
    /// footprint overlaps target pixel `(dx, dy)`'s destination cell,
    /// weighted by the *fractional* area of that overlap -- not just a
    /// flat count of whole source pixels. Used solely on the shrink path in
    /// [`Self::paint`] -- there's no upscaling call site for this, so
    /// unlike bilinear point-sampling it never needs to invent values
    /// between two real source pixels.
    ///
    /// Two refinements on top of a naive box filter, both aimed at the same
    /// "stair-stepped/jagged curve edges after shrinking" symptom:
    ///
    /// 1. **Fractional coverage, not truncated pixel ranges.** A first cut
    ///    of this filter picked whole-pixel sample ranges via integer
    ///    division (`dx * src_w / draw_w`), which snaps each destination
    ///    cell's sampling window to source-pixel boundaries instead of the
    ///    true (generally non-integer) source region it covers. That
    ///    quantizes exactly the sub-pixel gradient an edge needs to look
    ///    smooth, reproducing the same small stair-steps a shrink is
    ///    supposed to remove. This version instead computes the true
    ///    real-valued source span per destination pixel and weights each
    ///    touched source pixel by how much of it actually falls inside that
    ///    span (`1.0` for a fully covered interior pixel, a fraction for one
    ///    straddling the span's edge).
    /// 2. **Alpha-weighted colour.** `StatusNotifierItem` pixmaps routinely
    ///    carry meaningless RGB filler (often `0,0,0` black) behind fully
    ///    transparent (`alpha == 0`) pixels outside the icon's silhouette.
    ///    Giving every sampled pixel equal colour weight would blend that
    ///    filler straight into edge pixels' colour regardless of how
    ///    transparent the filler pixel was. Weighting each pixel's colour
    ///    contribution by `coverage * alpha` makes a fully transparent
    ///    neighbor contribute exactly zero colour, so only pixels that were
    ///    actually part of the icon's ink influence the blended colour --
    ///    the alpha channel itself still averages by plain coverage weight,
    ///    since coverage (how much of the footprint is opaque at all) is
    ///    exactly what antialiasing a shrunk edge needs.
    #[allow(clippy::too_many_lines)]
    fn area_average_pixel(
        source: &TrayPixmap,
        dx: i32,
        dy: i32,
        draw_width: i32,
        draw_height: i32,
    ) -> u32 {
        let scale_x = f64::from(source.width) / f64::from(draw_width);
        let scale_y = f64::from(source.height) / f64::from(draw_height);
        let x0f = f64::from(dx) * scale_x;
        let x1f = f64::from(dx + 1) * scale_x;
        let y0f = f64::from(dy) * scale_y;
        let y1f = f64::from(dy + 1) * scale_y;

        // Integer pixel range that could possibly overlap [x0f, x1f) x
        // [y0f, y1f) -- floor/ceil of the real-valued span, clamped so a
        // span that exactly lands on an integer boundary doesn't pull in an
        // extra empty-weight neighbor.
        #[allow(clippy::cast_possible_truncation)]
        let ix0 = x0f.floor() as i32;
        #[allow(clippy::cast_possible_truncation)]
        let ix1 = (x1f.ceil() as i32).max(ix0 + 1);
        #[allow(clippy::cast_possible_truncation)]
        let iy0 = y0f.floor() as i32;
        #[allow(clippy::cast_possible_truncation)]
        let iy1 = (y1f.ceil() as i32).max(iy0 + 1);

        let mut alpha_weight_sum = 0.0f64;
        let mut color_weight_sum = 0.0f64;
        let mut red_weighted = 0.0f64;
        let mut green_weighted = 0.0f64;
        let mut blue_weighted = 0.0f64;
        for sy in iy0..iy1 {
            // How much of source row `sy` (spanning [sy, sy+1)) falls
            // inside [y0f, y1f)?
            let y_overlap = (f64::from(sy + 1).min(y1f) - f64::from(sy).max(y0f)).max(0.0);
            if y_overlap <= 0.0 {
                continue;
            }
            for sx in ix0..ix1 {
                let x_overlap = (f64::from(sx + 1).min(x1f) - f64::from(sx).max(x0f)).max(0.0);
                if x_overlap <= 0.0 {
                    continue;
                }
                let coverage = x_overlap * y_overlap;
                let pixel = source.pixel_at_clamped(sx, sy);
                let a = f64::from(pixel >> 24 & 0xff);
                let coverage_alpha = coverage * a;
                alpha_weight_sum += coverage_alpha;
                color_weight_sum += coverage;
                red_weighted = coverage_alpha.mul_add(f64::from(pixel >> 16 & 0xff), red_weighted);
                green_weighted =
                    coverage_alpha.mul_add(f64::from(pixel >> 8 & 0xff), green_weighted);
                blue_weighted = coverage_alpha.mul_add(f64::from(pixel & 0xff), blue_weighted);
            }
        }
        // Alpha is a coverage-weighted average: how much of this footprint
        // was opaque at all, regardless of colour. `color_weight_sum` is the
        // total area sampled (always > 0 -- the loops above always cover
        // the full destination cell), so this never divides by zero.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let avg_alpha = ((alpha_weight_sum / color_weight_sum).round() as u32).min(255);
        // Colour channels divide by the alpha-and-coverage weight, not the
        // coverage alone -- an alpha-weighted mean. If every sampled pixel
        // was fully transparent there's no ink to average at all, so the
        // colour is moot; fall back to 0 rather than divide by zero.
        let avg_channel = |weighted: f64| -> u32 {
            if alpha_weight_sum <= 0.0 {
                return 0;
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let out = ((weighted / alpha_weight_sum).round() as u32).min(255);
            out
        };
        (avg_alpha << 24)
            | (avg_channel(red_weighted) << 16)
            | (avg_channel(green_weighted) << 8)
            | avg_channel(blue_weighted)
    }

    /// Choose the smallest square-ish pixmap at least `size` pixels wide or
    /// tall; if all are smaller, use the largest. `StatusNotifier` items often
    /// provide 16, 22, and 32 pixel variants, so this gives the expected icon
    /// at normal and `HiDPI` bar scales without a configuration knob.
    fn best_for(&self, size: i32) -> Option<&TrayPixmap> {
        let requested = size.max(1);
        self.pixmaps
            .iter()
            .filter(|p| p.width >= requested || p.height >= requested)
            .min_by_key(|p| p.width.max(p.height))
            .or_else(|| self.pixmaps.iter().max_by_key(|p| p.width.max(p.height)))
    }

    /// Composite one straight-ARGB source pixel (icon ink at coverage
    /// `alpha`) over a packed destination pixel, honoring `bg_alpha` (the
    /// bar's own window opacity, `0-255`) exactly the way
    /// `platform::draw::Color::blend_glyph` does for anti-aliased glyph
    /// edges -- the icon and the bar's glyphs share the same backbuffer, so
    /// they must agree on which packed format that buffer holds.
    ///
    /// `bg_alpha == 255`: the backbuffer is plain `Xrgb8888` (top byte
    /// ignored) and `dst`'s top byte is meaningless input -- returns a
    /// straight (non-premultiplied) RGB blend with the top byte forced to
    /// `0`, matching every other opaque-path write in this crate.
    ///
    /// `bg_alpha < 255`: the backbuffer is **premultiplied** `Argb8888`
    /// (`wl_shm`'s wire requirement -- see
    /// `platform::surface::SurfaceBootstrap::pack_bg`). `dst` therefore
    /// already has meaningful premultiplied RGB *and* alpha; compositing
    /// opaque icon ink at coverage `alpha` over it must Porter-Duff
    /// source-over in premultiplied space and produce a real, non-zero
    /// output alpha -- not the old behaviour of forcing alpha to `0` while
    /// leaving non-zero RGB behind, which violates the premultiplied
    /// invariant `channel <= alpha` and corrupts every partially-covered
    /// (antialiased/downscaled) icon pixel the compositor then blends.
    fn over(src: u32, dst: u32, alpha: u8, bg_alpha: u8) -> u32 {
        let a = u32::from(alpha);
        if bg_alpha == 255 {
            let inv = 255 - a;
            let r = ((src >> 16 & 0xff) * a + (dst >> 16 & 0xff) * inv) / 255;
            let g = ((src >> 8 & 0xff) * a + (dst >> 8 & 0xff) * inv) / 255;
            let b = ((src & 0xff) * a + (dst & 0xff) * inv) / 255;
            return (r << 16) | (g << 8) | b;
        }
        // Premultiplied source-over: `dst` is already premultiplied by its
        // own alpha, and the icon ink is fully opaque colour at coverage
        // `alpha`, so its premultiplied contribution is simply
        // `channel * alpha / 255` (no separate un-premultiply step needed
        // since the source pixel's colour channels are already straight,
        // i.e. un-premultiplied, per the `StatusNotifierItem` wire format).
        let dst_a = dst >> 24 & 0xff;
        let inv = 255 - a;
        let blend = |src_c: u32, dst_c: u32| -> u32 { (src_c * a + dst_c * inv) / 255 };
        let out_a = a + (dst_a * inv) / 255;
        let r = blend(src >> 16 & 0xff, dst >> 16 & 0xff);
        let g = blend(src >> 8 & 0xff, dst >> 8 & 0xff);
        let b = blend(src & 0xff, dst & 0xff);
        (out_a << 24) | (r << 16) | (g << 8) | b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::draw::Color;

    #[test]
    fn pixmap_rejects_bad_dimensions_and_lengths() {
        assert!(TrayPixmap::new(0, 1, vec![]).is_none());
        assert!(TrayPixmap::new(2, 2, vec![0; 3]).is_none());
        assert!(TrayPixmap::new(2, 2, vec![0; 4]).is_some());
    }

    #[test]
    fn paint_selects_the_smallest_sufficient_pixmap() {
        let small = TrayPixmap::new(16, 16, vec![0xffff_0000; 256]).unwrap();
        let large = TrayPixmap::new(32, 32, vec![0xff00_ff00; 1024]).unwrap();
        let icon = TrayIcon::new(vec![small, large]);
        let mut pixels = vec![Color::BLACK.to_xrgb8888(); 24 * 24];
        let mut canvas = Canvas::new(&mut pixels, 24, 24);
        icon.paint(&mut canvas, 0, 0, 20, 255);
        assert_eq!(pixels[0], Color::rgb(0, 255, 0).to_xrgb8888());
    }

    #[test]
    fn paint_alpha_composites_without_a_black_box() {
        let pixmap = TrayPixmap::new(1, 1, vec![0x80ff_0000]).unwrap();
        let icon = TrayIcon::new(vec![pixmap]);
        let mut pixels = vec![Color::rgb(0, 0, 255).to_xrgb8888()];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        icon.paint(&mut canvas, 0, 0, 1, 255);
        assert_eq!(pixels[0], 0x0080_007f);
    }

    #[test]
    fn paint_shrinks_a_source_larger_than_the_slot_to_fill_it_exactly() {
        // A 32x32 solid-red source painted into a 20x20 slot must be
        // downscaled (not clipped/overflowed): the whole 20x20 destination
        // square should end up red, and nothing outside it should be
        // touched -- this is the "icon too large for the slot" regression.
        let pixmap = TrayPixmap::new(32, 32, vec![0xffff_0000; 32 * 32]).unwrap();
        let icon = TrayIcon::new(vec![pixmap]);
        let mut pixels = vec![Color::BLACK.to_xrgb8888(); 24 * 24];
        let mut canvas = Canvas::new(&mut pixels, 24, 24);
        icon.paint(&mut canvas, 2, 2, 20, 255);

        let red = Color::rgb(255, 0, 0).to_xrgb8888();
        let black = Color::BLACK.to_xrgb8888();
        // Center of the slot is red.
        assert_eq!(pixels[12 * 24 + 12], red);
        // Just outside the 20x20 slot (slot spans x/y in 2..22) stays black.
        assert_eq!(pixels[0], black);
        assert_eq!(pixels[23 * 24 + 23], black);
    }

    #[test]
    fn shrinking_does_not_darken_edges_with_transparent_black_filler() {
        // Real `StatusNotifierItem` pixmaps often carry meaningless
        // `0,0,0` RGB filler behind fully-transparent (alpha=0) pixels
        // outside the icon's silhouette. A plain (non-alpha-weighted)
        // colour average would blend that black filler straight into every
        // edge pixel's *colour* during a shrink, muddying/darkening curved
        // and diagonal edges -- exactly the "still jagged/dark" symptom this
        // guards. The pixel's *coverage* (alpha) should still shrink to a
        // proper partial value either way -- that's correct antialiasing,
        // not the bug -- so this test isolates the colour channels by
        // comparing against the hand-computed alpha-weighted result rather
        // than expecting the naive "stays fully opaque" outcome.
        //
        // A 1-wide opaque red column next to a 1-wide fully-transparent
        // black column, downscaled 2x1 -> 1x1: alpha-weighted averaging
        // must keep the colour pure red (the transparent black contributes
        // zero colour weight), only the alpha itself becomes a 50%
        // coverage value.
        let argb = vec![
            0xffff_0000,
            0x0000_0000, // row 0: opaque red, transparent black
        ];
        let pixmap = TrayPixmap::new(2, 1, argb).unwrap();
        let icon = TrayIcon::new(vec![pixmap]);
        let mut pixels = vec![Color::rgb(255, 255, 255).to_xrgb8888(); 1];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        icon.paint(&mut canvas, 0, 0, 1, 255);

        // Weighted: colour averages to pure red (255,0,0) at ~50% coverage
        // (alpha rounds to 128/255), composited over white -> (255,127,127)
        // by the same integer `over` blend used everywhere else in this
        // module. An unweighted mean would instead average red's own
        // channel down before compositing, landing at a visibly darker,
        // muddier result.
        assert_eq!(pixels[0], Color::rgb(255, 127, 127).to_xrgb8888());
    }

    #[test]
    fn area_average_uses_fractional_source_coverage_not_truncated_pixel_ranges() {
        // A source span that lands mid-pixel must weight that straddled
        // pixel proportionally, not snap to whole source-pixel boundaries.
        // 3 source pixels (red, green, blue, all opaque) downscaled to a
        // single destination pixel: each source pixel contributes exactly
        // 1/3 coverage, so the correctly-weighted average is a flat
        // (85,85,85)-ish blend of all three -- a truncating-integer
        // implementation instead snaps its 1-pixel-wide sampling window to
        // just one of the three source pixels (whichever integer division
        // picked), reproducing visible banding/stair-stepping instead of a
        // smooth blend.
        let argb = vec![0xffff_0000, 0xff00_ff00, 0xff00_00ff];
        let pixmap = TrayPixmap::new(3, 1, argb).unwrap();
        let icon = TrayIcon::new(vec![pixmap]);
        let mut pixels = vec![Color::BLACK.to_xrgb8888(); 1];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        icon.paint(&mut canvas, 0, 0, 1, 255);

        // Each channel averages 255 from exactly one of the three source
        // pixels and 0 from the other two, weighted 1/3 each -> 85.
        assert_eq!(pixels[0], Color::rgb(85, 85, 85).to_xrgb8888());
    }

    #[test]
    fn paint_on_a_translucent_bar_writes_a_real_premultiplied_alpha_not_zero() {
        // Regression test for the actual reported bug: on a translucent bar
        // (bg_alpha < 255) the backbuffer is premultiplied Argb8888, and
        // every partially-covered icon pixel (downscale coverage or a
        // straight alpha<255 source pixel) must land with a real, non-zero
        // alpha byte -- not the old behaviour of always zeroing the top
        // byte, which corrupts the premultiplied invariant `channel <=
        // alpha` for the compositor and produces exactly the
        // distorted/muddy edge fringing this was reported as.
        let pixmap = TrayPixmap::new(1, 1, vec![0x80ff_0000]).unwrap(); // 50% red
        let icon = TrayIcon::new(vec![pixmap]);
        // Destination starts as a fully-opaque premultiplied blue pixel
        // (alpha=255, matching a fully opaque glass backdrop) so the blend
        // math has a known non-degenerate `dst` to combine with.
        let mut pixels = vec![0xff00_00ffu32];
        let mut canvas = Canvas::new(&mut pixels, 1, 1);
        icon.paint(&mut canvas, 0, 0, 1, 128); // translucent bar

        let out = pixels[0];
        let out_a = out >> 24 & 0xff;
        // Coverage alpha is 0x80 (128); output alpha must reflect that
        // real coverage, not 0.
        assert!(out_a > 0, "alpha byte must not be zeroed on a translucent bar");
        // Premultiplied invariant: every colour channel must stay <= alpha,
        // never exceed it (the bug produced non-zero RGB behind a zero
        // alpha, i.e. maximally violating this).
        let r = out >> 16 & 0xff;
        let g = out >> 8 & 0xff;
        let b = out & 0xff;
        assert!(r <= out_a && g <= out_a && b <= out_a);
    }
}
