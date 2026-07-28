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

    /// A shade of `self` that stays visibly distinct from it, stepping away
    /// from `contrast` by `t` where there is room and toward it where there
    /// is not.
    ///
    /// This exists because plain [`Color::mix`] with a negative factor is
    /// not safe for deriving UI chrome. "Recede from the text" means darken
    /// on a dark theme -- but on a pure-white background there is nothing
    /// darker to clamp *away* to, so the mix returns white again and the
    /// chrome silently vanishes into the background. (Caught by test, not
    /// by inspection: `derived_chrome_stays_distinguishable_on_a_light_
    /// palette` failed on exactly that case.)
    ///
    /// Stepping toward `contrast` instead whenever the away-step had no
    /// effect guarantees a distinct result for every palette, including the
    /// pure black and pure white extremes.
    #[must_use]
    pub fn recede_from(self, contrast: Self, t: f32) -> Self {
        let away = self.mix(contrast, -t);
        if away == self {
            self.mix(contrast, t)
        } else {
            away
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
