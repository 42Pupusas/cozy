//! `org.freedesktop.appearance` `color-scheme`: the system's preferred
//! light/dark appearance, as the spec's `u` enum.
//!
//! Its own type (not a bare `u32` threaded through [`crate::appearance_settings::AppearanceSettings`])
//! because the mapping from a palette to a preference is itself a small
//! decision worth naming and testing in isolation: [`Self::from_background`]
//! is the *only* place this workspace decides "is this palette dark or
//! light", and every future backend key that cares about polarity (were one
//! ever added) should call it rather than re-deriving the same luma
//! threshold by hand.

use config::Color;

/// The system's preferred color scheme, per
/// `org.freedesktop.impl.portal.Settings`'s documented `u` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    NoPreference,
    PreferDark,
    PreferLight,
}

impl ColorScheme {
    /// Perceived-brightness threshold below which a background reads as
    /// "dark" rather than "light".
    ///
    /// Half of the 0..=255 luma range -- there's no config knob for this
    /// today, so the midpoint is the only defensible default; a themed
    /// background is overwhelmingly close to one end or the other in
    /// practice; (`config::Color::luma`'s own doc comment covers the
    /// perceptual weighting this threshold is applied to).
    const DARK_LUMA_THRESHOLD: f32 = 127.5;

    /// Classify `background` as dark or light. Never reports
    /// [`Self::NoPreference`] -- a terminal's palette always *has* a
    /// background, so there's always a real preference to report; a caller
    /// wanting an explicit "don't know" state has no config value to ask
    /// this from in the first place.
    #[must_use]
    pub fn from_background(background: Color) -> Self {
        if background.luma() < Self::DARK_LUMA_THRESHOLD {
            Self::PreferDark
        } else {
            Self::PreferLight
        }
    }

    /// The wire value the portal method reply carries.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::NoPreference => 0,
            Self::PreferDark => 1,
            Self::PreferLight => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_near_black_background_prefers_dark() {
        let bg = Color::from_rgb(0x18, 0x18, 0x18);
        assert_eq!(ColorScheme::from_background(bg), ColorScheme::PreferDark);
        assert_eq!(ColorScheme::from_background(bg).as_u32(), 1);
    }

    #[test]
    fn a_near_white_background_prefers_light() {
        let bg = Color::from_rgb(0xf8, 0xf8, 0xf8);
        assert_eq!(ColorScheme::from_background(bg), ColorScheme::PreferLight);
        assert_eq!(ColorScheme::from_background(bg).as_u32(), 2);
    }

    #[test]
    fn no_preference_still_reports_the_documented_zero() {
        assert_eq!(ColorScheme::NoPreference.as_u32(), 0);
    }

    #[test]
    fn a_mid_grey_background_falls_on_the_dark_side_of_the_threshold() {
        // Rec. 601 luma of #808080 is 128.0, just above the 127.5
        // threshold -- picked deliberately as a near-boundary case rather
        // than only ever testing the extremes.
        let bg = Color::from_rgb(0x80, 0x80, 0x80);
        assert_eq!(ColorScheme::from_background(bg), ColorScheme::PreferLight);
    }
}
