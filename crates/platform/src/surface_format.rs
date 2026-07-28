//! How a colour becomes a word in *this* surface's pixel buffer.
//!
//! A `wl_shm` surface is either `Xrgb8888` (the compositor ignores the top
//! byte) or `Argb8888` (the top byte **is** the alpha the compositor blends
//! with what is behind the window). Every drawing primitive therefore has to
//! pack differently depending on which buffer it is writing into -- and
//! getting it wrong is silent: pack a plain `0x00RRGGBB` into an `Argb8888`
//! buffer and you have written alpha 0, an invisible rectangle.
//!
//! Before this type, that decision was duplicated per *call site*: `Canvas`
//! offered `fill_rect` (opaque packing) beside `fill_rect_alpha` (translucent
//! packing) and left every caller to pick, while `fill_round_rect`, `clear`
//! and `blend_pixel` had no translucent form at all. The browser's window
//! painter alone had 21 such calls. That is the wrong owner: whether a pixel
//! needs an alpha byte is a property of the *surface*, fixed for its
//! lifetime, not a property of each individual rectangle someone draws on it.
//!
//! So the knowledge lives here, once, and [`Canvas`](crate::draw::Canvas)
//! holds one. Primitives ask it to pack; nobody else decides.
//!
//! # The "translucent glass, solid paint" contract
//!
//! `bg_alpha` is the *window's* opacity, not the paint's. A window at 80%
//! shows the desktop through its background, but its text, buttons and
//! selection highlights stay fully opaque -- otherwise the wallpaper bleeds
//! through the filenames and the window becomes unreadable. So solid UI
//! chrome is packed premultiplied at **full** alpha even on a translucent
//! surface; only the surface's own cleared background carries `bg_alpha`.
//! [`Self::pack_paint`] and [`Self::pack_glass`] are those two cases, and
//! keeping them as two named methods is what stops the distinction from
//! being re-litigated at each call site.

use crate::draw::Color;

/// The pixel packing rule for one surface.
///
/// `Copy` and pointer-sized: primitives read it per pixel run, so it must
/// cost nothing to pass around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceFormat {
    /// The window's background opacity, `255` = fully opaque.
    ///
    /// Stored rather than a bool because [`Self::pack_glass`] needs the
    /// actual value, and because `255` is exactly the condition that selects
    /// the cheaper opaque path everywhere else.
    bg_alpha: u8,
}

impl SurfaceFormat {
    /// The opaque `Xrgb8888` surface -- the default, and byte-for-byte the
    /// behaviour every primitive had before this type existed.
    #[must_use]
    pub const fn opaque() -> Self {
        Self { bg_alpha: 255 }
    }

    /// A surface at `bg_alpha/255` background opacity.
    ///
    /// `255` yields exactly [`Self::opaque`], so callers converting a
    /// configured `f64` opacity need no branch of their own.
    #[must_use]
    pub const fn with_bg_alpha(bg_alpha: u8) -> Self {
        Self { bg_alpha }
    }

    /// Build from a configured opacity fraction, clamping out-of-range values
    /// rather than wrapping them into a nonsense alpha.
    ///
    /// Centralised here because every client had its own copy of
    /// `(opacity.clamp(0.0, 1.0) * 255.0).round() as u8`, and a rounding
    /// disagreement between two of them would show up as one window being
    /// visibly a shade off from its neighbour.
    #[must_use]
    pub fn from_opacity(opacity: f64) -> Self {
        // Float source with no lossless-widening path; the clamp above bounds
        // the product to 0.0..=255.0, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bg_alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self { bg_alpha }
    }

    /// The window's background opacity.
    #[must_use]
    pub const fn bg_alpha(&self) -> u8 {
        self.bg_alpha
    }

    /// Whether this surface needs the `Argb8888` buffer format.
    ///
    /// The buffer format is fixed when the surface is created, so this is
    /// what the window bootstrap consults to choose between `Argb8888` and
    /// the cheaper `Xrgb8888` the compositor never has to blend.
    #[must_use]
    pub const fn needs_alpha(&self) -> bool {
        self.bg_alpha < 255
    }

    /// Pack solid paint: opaque ink, whatever the window's own opacity.
    ///
    /// This is the right call for essentially all UI chrome -- panels,
    /// buttons, selection highlights, separators. On an opaque surface it is
    /// the plain `0x00RRGGBB` word; on a translucent one it is premultiplied
    /// at full alpha, which is the identity on the colour channels, so the
    /// paint reads as solid rather than inheriting the window's glass.
    #[must_use]
    pub fn pack_paint(&self, color: Color) -> u32 {
        if self.bg_alpha == 255 {
            color.to_xrgb8888()
        } else {
            color.premultiplied_argb8888(255)
        }
    }

    /// Pack the surface's own background, which *does* carry the window
    /// opacity -- this is the "glass" the desktop shows through.
    #[must_use]
    pub fn pack_glass(&self, color: Color) -> u32 {
        if self.bg_alpha == 255 {
            color.to_xrgb8888()
        } else {
            color.premultiplied_argb8888(self.bg_alpha)
        }
    }

    /// Composite `color` at `coverage/255` over the already-packed
    /// destination pixel `dst`, returning the new packed word.
    ///
    /// This is the anti-aliasing operation behind rounded corners. The two
    /// paths are not stylistic variants -- they are different algebra:
    ///
    /// * Opaque surface: `dst` is straight RGB, so this is the original
    ///   linear interpolation, byte-identical to the pre-existing blend.
    /// * Translucent surface: `dst` is **premultiplied**, so unpacking it as
    ///   straight RGB (what the old code did) would darken every corner
    ///   pixel by the window's alpha. Premultiplied "source over
    ///   destination" is `src + dst * (1 - coverage)`, computed per channel
    ///   including alpha, which composites correctly whether what lies
    ///   beneath is the window's glass or another solid fill.
    #[must_use]
    pub fn blend_over(&self, dst: u32, color: Color, coverage: u8) -> u32 {
        if self.bg_alpha == 255 {
            let background = Color::rgb(
                ((dst >> 16) & 0xff) as u8,
                ((dst >> 8) & 0xff) as u8,
                (dst & 0xff) as u8,
            );
            return background.blend_opaque_to(color, coverage);
        }
        // Premultiplied source: opaque ink at `coverage` alpha.
        let c = u32::from(coverage);
        let src_a = c;
        let src_r = (u32::from(color.r) * c) / 255;
        let src_g = (u32::from(color.g) * c) / 255;
        let src_b = (u32::from(color.b) * c) / 255;
        // `div255(x) == x / 255` exactly over the range these products
        // occupy, without a hardware divide -- the same trick
        // `Color::blend_opaque_color` documents.
        let div255 = |x: u32| (x + 1 + (x >> 8)) >> 8;
        let inv = 255 - c;
        let out_a = src_a + div255(((dst >> 24) & 0xff) * inv);
        let out_r = src_r + div255(((dst >> 16) & 0xff) * inv);
        let out_g = src_g + div255(((dst >> 8) & 0xff) * inv);
        let out_b = src_b + div255((dst & 0xff) * inv);
        // Each channel is a convex combination of two values <= 255, so no
        // term can exceed 255; the mask is belt-and-braces against a future
        // edit, not a live concern.
        ((out_a & 0xff) << 24) | ((out_r & 0xff) << 16) | ((out_g & 0xff) << 8) | (out_b & 0xff)
    }
}

/// Opaque is the default, so existing surfaces keep their behaviour without
/// naming a format at all.
impl Default for SurfaceFormat {
    fn default() -> Self {
        Self::opaque()
    }
}

#[cfg(test)]
mod tests {
    use super::SurfaceFormat;
    use crate::draw::Color;

    /// The whole point of the opaque path: it must be indistinguishable from
    /// the packing that existed before this type, or every already-shipped
    /// window changes appearance.
    #[test]
    fn the_opaque_surface_packs_exactly_as_plain_xrgb() {
        let fmt = SurfaceFormat::opaque();
        for color in [
            Color::rgb(0, 0, 0),
            Color::rgb(255, 255, 255),
            Color::rgb(0x1f, 0x1f, 0x28),
            Color::rgb(0xc5, 0xc9, 0xc5),
        ] {
            assert_eq!(fmt.pack_paint(color), color.to_xrgb8888());
            assert_eq!(fmt.pack_glass(color), color.to_xrgb8888());
        }
    }

    /// An opaque surface must have a zero top byte; a translucent one must
    /// not, since that byte is what the compositor reads as alpha. This is
    /// the exact bug the type exists to prevent.
    #[test]
    fn only_the_translucent_surface_writes_an_alpha_byte() {
        let color = Color::rgb(0x80, 0x90, 0xa0);
        assert_eq!(SurfaceFormat::opaque().pack_paint(color) >> 24, 0);
        assert_eq!(
            SurfaceFormat::with_bg_alpha(204).pack_paint(color) >> 24,
            255,
            "solid paint stays solid on a translucent window"
        );
        assert_eq!(
            SurfaceFormat::with_bg_alpha(204).pack_glass(color) >> 24,
            204,
            "the background is what carries the window opacity"
        );
    }

    /// Paint and glass differ *only* on a translucent surface. If they ever
    /// diverge on an opaque one, opaque windows have silently changed.
    #[test]
    fn paint_and_glass_agree_when_opaque_and_differ_when_not() {
        let color = Color::rgb(0x30, 0x40, 0x50);
        let opaque = SurfaceFormat::opaque();
        assert_eq!(opaque.pack_paint(color), opaque.pack_glass(color));

        let glass = SurfaceFormat::with_bg_alpha(128);
        assert_ne!(glass.pack_paint(color), glass.pack_glass(color));
    }

    /// Premultiplication must actually scale the colour channels, not just
    /// stamp an alpha byte on top of full-brightness RGB -- that mistake
    /// produces a washed-out, too-bright window.
    #[test]
    fn glass_premultiplies_the_colour_channels() {
        let packed = SurfaceFormat::with_bg_alpha(128).pack_glass(Color::rgb(200, 100, 50));
        assert_eq!((packed >> 24) & 0xff, 128);
        assert_eq!((packed >> 16) & 0xff, (200 * 128) / 255);
        assert_eq!((packed >> 8) & 0xff, (100 * 128) / 255);
        assert_eq!(packed & 0xff, (50 * 128) / 255);
    }

    /// Full opacity is the identity for premultiplication, so `with_bg_alpha(255)`
    /// must be the opaque surface rather than a near-miss of it.
    #[test]
    fn full_alpha_is_exactly_the_opaque_surface() {
        assert_eq!(SurfaceFormat::with_bg_alpha(255), SurfaceFormat::opaque());
        assert_eq!(SurfaceFormat::from_opacity(1.0), SurfaceFormat::opaque());
        assert!(!SurfaceFormat::opaque().needs_alpha());
        assert!(SurfaceFormat::with_bg_alpha(254).needs_alpha());
    }

    /// Out-of-range opacity is clamped, never wrapped: a `1.5` that wrapped
    /// to a low alpha would render an all-but-invisible window.
    #[test]
    fn an_out_of_range_opacity_clamps_rather_than_wrapping() {
        assert_eq!(SurfaceFormat::from_opacity(1.5).bg_alpha(), 255);
        assert_eq!(SurfaceFormat::from_opacity(f64::INFINITY).bg_alpha(), 255);
        assert_eq!(SurfaceFormat::from_opacity(-0.5).bg_alpha(), 0);
        assert_eq!(SurfaceFormat::from_opacity(f64::NEG_INFINITY).bg_alpha(), 0);
    }

    /// The config files in this repo all use 0.80; check it lands on the
    /// alpha the other clients already compute by hand.
    #[test]
    fn the_configured_eighty_percent_matches_the_other_clients() {
        assert_eq!(SurfaceFormat::from_opacity(0.80).bg_alpha(), 204);
    }

    /// Blending at the extremes must be exact, not merely close: a corner
    /// pixel at zero coverage that shifted the destination would leave a
    /// visible seam around every rounded shape.
    #[test]
    fn zero_coverage_leaves_the_destination_untouched() {
        let ink = Color::rgb(255, 0, 0);
        for fmt in [
            SurfaceFormat::opaque(),
            SurfaceFormat::with_bg_alpha(204),
            SurfaceFormat::with_bg_alpha(0),
        ] {
            let dst = fmt.pack_glass(Color::rgb(0x1f, 0x1f, 0x28));
            assert_eq!(fmt.blend_over(dst, ink, 0), dst, "{fmt:?}");
        }
    }

    /// Full coverage must land on solid paint -- the interior of a rounded
    /// shape and the pixel just inside its arc have to be the same colour, or
    /// the corners read as a lighter fringe.
    #[test]
    fn full_coverage_yields_the_same_word_as_a_solid_fill() {
        let ink = Color::rgb(0x7e, 0x9c, 0xd8);
        for fmt in [SurfaceFormat::opaque(), SurfaceFormat::with_bg_alpha(204)] {
            let dst = fmt.pack_glass(Color::rgb(0x1f, 0x1f, 0x28));
            assert_eq!(
                fmt.blend_over(dst, ink, 255),
                fmt.pack_paint(ink),
                "{fmt:?}"
            );
        }
    }

    /// On a translucent surface, compositing ink over the glass must *raise*
    /// the alpha byte: the covered part of the pixel is now opaque paint. The
    /// old straight-RGB blend left alpha alone, which is what made rounded
    /// corners on a translucent window fade out.
    #[test]
    fn blending_over_glass_raises_the_alpha_toward_opaque() {
        let fmt = SurfaceFormat::with_bg_alpha(128);
        let dst = fmt.pack_glass(Color::rgb(0x20, 0x20, 0x20));
        let blended = fmt.blend_over(dst, Color::rgb(255, 255, 255), 128);
        let dst_a = (dst >> 24) & 0xff;
        let out_a = (blended >> 24) & 0xff;
        assert!(
            out_a > dst_a,
            "half-covering opaque ink should make the pixel more opaque: {dst_a} -> {out_a}"
        );
        assert!(out_a < 255, "and not fully opaque at half coverage");
    }

    /// A premultiplied pixel is only valid if no colour channel exceeds the
    /// alpha channel; violating that produces colours the compositor renders
    /// as garbage. Sweep coverages and alphas to be sure the algebra holds.
    #[test]
    fn every_blend_result_stays_a_valid_premultiplied_pixel() {
        for bg_alpha in [1_u8, 64, 128, 204, 254] {
            let fmt = SurfaceFormat::with_bg_alpha(bg_alpha);
            let dst = fmt.pack_glass(Color::rgb(0xc5, 0xc9, 0xc5));
            for coverage in 0..=255_u8 {
                let out = fmt.blend_over(dst, Color::rgb(255, 200, 100), coverage);
                let a = (out >> 24) & 0xff;
                for shift in [16, 8, 0] {
                    let channel = (out >> shift) & 0xff;
                    assert!(
                        channel <= a,
                        "channel {channel} exceeds alpha {a} at bg_alpha={bg_alpha} coverage={coverage}"
                    );
                }
            }
        }
    }

    /// Coverage should move the result monotonically from destination to ink.
    /// A non-monotonic blend shows up as banding in the corner arcs.
    #[test]
    fn more_coverage_never_moves_further_from_the_ink() {
        let fmt = SurfaceFormat::with_bg_alpha(204);
        let dst = fmt.pack_glass(Color::rgb(0, 0, 0));
        let mut previous = 0;
        for coverage in 0..=255_u8 {
            let red = (fmt.blend_over(dst, Color::rgb(255, 0, 0), coverage) >> 16) & 0xff;
            assert!(red >= previous, "red went backwards at coverage {coverage}");
            previous = red;
        }
    }
}
