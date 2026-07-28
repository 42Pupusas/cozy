//! [`Density`] -- how tightly the browser packs content, relative to the
//! design guide's published table.
//!
//! `Spacing` encodes the guide faithfully and is tested against its literal
//! pixel table; that contract is worth keeping intact. But the guide's numbers
//! were drawn for a mockup with **file-type icons** in every row: a 49px row
//! is generous because it has a 20px glyph to seat. We do not draw icons yet,
//! so those proportions leave rows looking airy and the type looking oversized
//! for a dense file list.
//!
//! Rather than editing the guide constants (which would make `Spacing` lie
//! about what it encodes) or scattering `* 0.6` at the call sites (which is
//! how a rhythm turns back into magic numbers), the deviation lives here as
//! one named, tested policy. Two consumers read it -- [`crate::theme::Theme`]
//! for geometry and [`crate::typography::Typography`] for type size -- so
//! "how dense is the browser" has exactly one owner.

/// The browser's packing policy: multipliers against the guide's dimensions.
///
/// `Copy` and const-constructible so the shipped policy is a `const` rather
/// than something assembled at startup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Density {
    /// Applied to row and band heights.
    row: f32,
    /// Applied to padding and internal gaps.
    pad: f32,
    /// Applied to every type tier.
    text: f32,
}

impl Density {
    /// The guide exactly as published -- every multiplier 1.0.
    ///
    /// Kept as the reference point the tests compare against, so "how far
    /// have we deviated" is answerable rather than implicit. Not shipped: the
    /// browser runs [`Self::COMPACT`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub const GUIDE: Self = Self {
        row: 1.0,
        pad: 1.0,
        text: 1.0,
    };

    /// What the browser ships: tighter rows, tighter padding, smaller type.
    ///
    /// The three move together on purpose. Shrinking the type alone would
    /// leave the same airy rows with smaller text floating in them, and
    /// tightening rows alone would crowd full-size glyphs against their own
    /// separators. Density is one perceptual property, so it is one number
    /// set here rather than three independently-tuned knobs.
    pub const COMPACT: Self = Self {
        // 49px -> ~30px. The guide's row seats an icon we do not draw; a
        // text-only row needs roughly its line height plus a little air.
        row: 0.62,
        // 12px -> 8px, landing on the rhythm's S step rather than between
        // steps, so the tightened padding is still one of the guide's values.
        pad: 0.66,
        // Filenames stop being the configured size exactly (see
        // `Typography::BODY`) -- a file list is scanned, not read, and the
        // configured size is chosen for terminal prose.
        text: 0.88,
    };

    /// Scale a guide row or band height.
    ///
    /// Floors at 1: a row that rounds to zero height renders an invisible
    /// list, which is worse than a cramped one.
    #[must_use]
    pub fn row(self, guide_px: i32) -> i32 {
        Self::apply(guide_px, self.row)
    }

    /// Scale a guide padding or gap.
    #[must_use]
    pub fn pad(self, guide_px: i32) -> i32 {
        Self::apply(guide_px, self.pad)
    }

    /// Scale a type-tier multiplier.
    ///
    /// Returns the multiplier rather than a pixel size, because
    /// [`crate::typography::Typography`] composes it with the configured font
    /// size and the output scale; doing that arithmetic here would duplicate
    /// the part that already has a home.
    #[must_use]
    pub fn text(self, tier_mult: f32) -> f32 {
        tier_mult * self.text
    }

    /// One rounded, floored multiplication, shared so `row` and `pad` cannot
    /// drift in how they handle the degenerate cases.
    fn apply(guide_px: i32, factor: f32) -> i32 {
        if guide_px <= 0 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
        let scaled = ((guide_px as f32) * factor).round() as i32;
        scaled.max(1)
    }
}

impl Default for Density {
    /// [`Self::COMPACT`] -- the shipped policy, so a `Density::default()` in
    /// a test or a new call site gets what the user actually sees.
    fn default() -> Self {
        Self::COMPACT
    }
}

#[cfg(test)]
mod tests {
    use super::Density;

    /// The guide policy must be a true no-op, or it is useless as a reference
    /// point for what the compact policy changes.
    #[test]
    fn the_guide_policy_changes_nothing() {
        let d = Density::GUIDE;
        for px in [1, 2, 12, 30, 49, 58, 210] {
            assert_eq!(d.row(px), px, "row {px} must pass through");
            assert_eq!(d.pad(px), px, "pad {px} must pass through");
        }
        for mult in [0.78_f32, 0.85, 1.0, 1.15] {
            assert!((d.text(mult) - mult).abs() < f32::EPSILON);
        }
    }

    /// The shipped policy must actually tighten all three axes -- that is the
    /// entire reason it exists.
    #[test]
    fn the_compact_policy_tightens_every_axis() {
        let d = Density::COMPACT;
        assert!(d.row(49) < 49, "rows must get shorter");
        assert!(d.pad(12) < 12, "padding must get tighter");
        assert!(d.text(1.0) < 1.0, "type must get smaller");
    }

    /// Pinned so a future tweak to the multipliers is a deliberate, visible
    /// edit rather than a silent reflow of every window.
    #[test]
    fn the_compact_geometry_lands_on_its_intended_numbers() {
        let d = Density::COMPACT;
        assert_eq!(d.row(49), 30, "the list row");
        assert_eq!(d.pad(12), 8, "padding lands on the rhythm's S step");
    }

    /// Nothing may collapse to zero, at any guide value, or the layout
    /// silently loses a row height or a divider.
    #[test]
    fn nothing_rounds_away_to_nothing() {
        for d in [Density::GUIDE, Density::COMPACT, Density::default()] {
            for px in 1..=64 {
                assert!(d.row(px) > 0, "row {px} vanished");
                assert!(d.pad(px) > 0, "pad {px} vanished");
            }
        }
    }

    /// A non-positive input is genuinely absent (a zero-height strip is a
    /// legitimate request) and must stay absent rather than being floored up
    /// into a stray one-pixel band.
    #[test]
    fn an_absent_dimension_stays_absent() {
        for d in [Density::GUIDE, Density::COMPACT] {
            assert_eq!(d.row(0), 0);
            assert_eq!(d.pad(0), 0);
            assert_eq!(d.row(-5), 0);
        }
    }

    /// Scaling must be monotonic: a taller guide row can never come out
    /// shorter than a smaller one.
    #[test]
    fn scaling_preserves_order() {
        let d = Density::COMPACT;
        let mut previous = 0;
        for px in 1..=256 {
            let scaled = d.row(px);
            assert!(scaled >= previous, "row scaling reversed at {px}");
            previous = scaled;
        }
    }

    /// The default is the shipped policy, not the guide -- a `default()` that
    /// silently meant "untightened" would make every new call site wrong.
    #[test]
    fn the_default_is_the_shipped_policy() {
        assert_eq!(Density::default(), Density::COMPACT);
    }
}
