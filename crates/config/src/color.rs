//! 24-bit RGB color, parsed from `#rrggbb` hex strings.

/// A 24-bit RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    #[must_use]
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rrggbb` (with or without leading `#`).
    #[must_use]
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some(Self { r, g, b })
    }

    /// Blend `t` of the way from `self` toward `other`.
    ///
    /// A *negative* `t` moves the same distance in the opposite direction --
    /// away from `other`, toward black or white as the channel allows. That
    /// is what makes this useful for deriving UI chrome from a palette: a
    /// file browser needs a header strip that recedes behind the list, and
    /// "recedes" means darker on a dark theme but lighter on a light one.
    /// Mixing the background *away from* the foreground gets both from one
    /// expression, where a hardcoded dark slate would vanish on a light
    /// palette.
    ///
    /// Results are clamped per channel, so an extreme `t` saturates rather
    /// than wrapping around to a wildly wrong hue.
    #[must_use]
    pub fn mix(self, other: Self, t: f32) -> Self {
        // Per channel: self + (other - self) * t, in f32 so a negative t
        // and the clamp both behave, then back to u8. The clamp to 0..=255
        // before the cast is what makes the truncation allow-free and
        // guarantees no wraparound.
        // The clamp to 0..=255 happens *before* the cast, so the value is
        // always in range for u8: truncation and sign loss cannot actually
        // occur, and the casts are the same deliberate pattern the opacity
        // conversions in the bar and launcher use.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let blend = |a: u8, b: u8| -> u8 {
            let a = f32::from(a);
            let b = f32::from(b);
            (b - a).mul_add(t, a).clamp(0.0, 255.0).round() as u8
        };
        Self {
            r: blend(self.r, other.r),
            g: blend(self.g, other.g),
            b: blend(self.b, other.b),
        }
    }

    /// The largest per-channel headroom [`Self::recede_from`] will scale its
    /// step against, so a subtle surface stays subtle on every palette.
    ///
    /// Without a bound the step is a fraction of the distance to the
    /// contrast colour, which is only small when the two are already close.
    /// On a near-black theme (background `#181616`, foreground `#c5c9c5`)
    /// that distance is ~197 of 255, so even a modest `t` overshoots past
    /// the bottom of the range and clamps to pure black -- a ~22-point drop
    /// presented as a nudge, and a direct violation of the design guide's
    /// rule to keep large surfaces near-black rather than pure black. The
    /// guide's own surfaces sit 4-7 points apart, so scaling against 64
    /// keeps `t = 0.35` at ~8 points: clearly a different surface, still
    /// unmistakably the same near-black family.
    const SURFACE_HEADROOM: f32 = 64.0;

    /// A shade of `self` that stays visibly distinct from it, stepping away
    /// from `contrast` where there is room and toward it where there is not.
    ///
    /// This exists because plain [`Color::mix`] with a negative factor is
    /// not safe for deriving UI chrome, for two separate reasons:
    ///
    /// * On a pure-white background there is nothing darker to clamp *away*
    ///   to, so the mix returns white again and the chrome silently
    ///   vanishes. (Caught by test, not inspection:
    ///   `derived_chrome_stays_distinguishable_on_a_light_palette` failed on
    ///   exactly that case.) Stepping toward `contrast` whenever the away
    ///   step had no effect guarantees a distinct result at both extremes.
    ///
    /// * A fraction of the *distance to the foreground* is the wrong
    ///   magnitude for a surface tint -- see [`Self::SURFACE_HEADROOM`].
    ///   `t` is therefore a fraction of the room actually available in the
    ///   direction of travel, capped, rather than of the full span.
    ///
    /// The step is at least one point wherever any room exists, so subtle
    /// never rounds down into invisible.
    #[must_use]
    pub fn recede_from(self, contrast: Self, t: f32) -> Self {
        let away = self.step_from(contrast, t, false);
        if away == self {
            self.step_from(contrast, t, true)
        } else {
            away
        }
    }

    /// One bounded per-channel step, either toward `contrast` or away from
    /// it. Shared by both arms of [`Self::recede_from`] so the fallback is
    /// as gentle as the primary path -- a light theme should not get a grey
    /// header just because it had no room to brighten.
    fn step_from(self, contrast: Self, t: f32, toward: bool) -> Self {
        // Clamped to 0..=255 before the cast, so truncation and sign loss
        // cannot occur -- the same deliberate pattern as `mix`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let shift = |a: u8, b: u8| -> u8 {
            // Which way is away-from-b for this channel; ties are treated as
            // darken, matching the dark-theme common case.
            let darken = if toward { b < a } else { b >= a };
            let room = f32::from(if darken { a } else { 255 - a });
            let magnitude = t.abs() * room.min(Self::SURFACE_HEADROOM);
            // Round up, so a small-but-real headroom still moves a point
            // instead of rounding away to nothing.
            let magnitude = if room > 0.0 { magnitude.max(1.0) } else { 0.0 };
            let signed = if darken { -magnitude } else { magnitude };
            (f32::from(a) + signed).clamp(0.0, 255.0).round() as u8
        };
        Self {
            r: shift(self.r, contrast.r),
            g: shift(self.g, contrast.g),
            b: shift(self.b, contrast.b),
        }
    }

    /// Perceived brightness, 0.0..=255.0.
    ///
    /// Rec. 601 coefficients rather than a plain channel average, because the
    /// eye is far more sensitive to green than to blue: averaging would call
    /// a saturated blue and a saturated green equally bright, which is exactly
    /// the misjudgement this is used to avoid.
    #[must_use]
    pub fn luma(self) -> f32 {
        // Written as fused multiply-adds rather than the textbook sum: clippy's
        // `suboptimal_flops` asks for it, and each `mul_add` keeps full
        // precision on the intermediate instead of rounding twice.
        let red = 0.299 * f32::from(self.r);
        let with_green = 0.587_f32.mul_add(f32::from(self.g), red);
        0.114_f32.mul_add(f32::from(self.b), with_green)
    }

    /// `self` re-scaled to `target`'s perceived brightness, keeping its hue.
    ///
    /// For distinguishing two UI roles by *colour* rather than by lightness.
    /// Two inks of different brightness, washed over one background at the
    /// same strength, produce two fills that differ mainly in how light they
    /// are -- which the eye reads as "the same colour at two opacities", not
    /// as two different colours. Equalising luma first leaves hue as the only
    /// difference, so the distinction survives at low wash strengths where a
    /// lightness difference would just look like inconsistent alpha.
    ///
    /// Scales all three channels by one factor, so the ratios between them --
    /// the hue -- are preserved. A channel that would exceed 255 clamps, which
    /// desaturates slightly at the very top of the range; that is preferable
    /// to wrapping, and the inks this is used on are mid-tones.
    ///
    /// A black `self` has no hue to preserve and no ratio to scale, so it is
    /// returned unchanged rather than being divided by zero.
    #[must_use]
    pub fn at_luma_of(self, target: Self) -> Self {
        let current = self.luma();
        if current <= f32::EPSILON {
            return self;
        }
        let factor = target.luma() / current;
        // Clamped before the cast, so truncation and sign loss cannot occur --
        // the same deliberate pattern as `mix` and `step_from`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scale = |c: u8| -> u8 { (f32::from(c) * factor).clamp(0.0, 255.0).round() as u8 };
        Self {
            r: scale(self.r),
            g: scale(self.g),
            b: scale(self.b),
        }
    }

    /// Pack as `0x00_rr_gg_bb`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

impl std::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}
