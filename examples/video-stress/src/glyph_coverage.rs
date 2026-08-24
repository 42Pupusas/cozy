//! Which glyphs a subcell mode needs, and whether a face supplies them.
//!
//! A face that lacks one mask renders that mask as `.notdef`, the "tofu"
//! box. The gap shows only on the frames that produce the absent masks, so
//! a partial coverage looks like damage on some shapes and not others.

use crate::subcell_grid::SubcellGrid;
use font::FontFace;

/// The glyph set one subcell mode draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphCoverage {
    mode: SubcellGrid,
}

impl GlyphCoverage {
    /// Cover the glyph set of `mode`.
    #[must_use]
    pub const fn new(mode: SubcellGrid) -> Self {
        Self { mode }
    }

    /// The mode this coverage examines.
    #[must_use]
    pub const fn mode(self) -> SubcellGrid {
        self.mode
    }

    /// Every distinct non-blank glyph the mode can emit, in code order.
    #[must_use]
    pub fn required(self) -> Vec<char> {
        let masks = 1u8 << self.mode.count();
        let mut glyphs: Vec<char> = (0..masks)
            .map(|mask| self.mode.glyph(mask))
            .filter(|glyph| *glyph != ' ')
            .collect();
        glyphs.sort_unstable();
        glyphs.dedup();
        glyphs
    }

    /// The required glyphs that `face` renders as `.notdef`.
    #[must_use]
    pub fn missing(self, face: &FontFace) -> Vec<char> {
        self.required()
            .into_iter()
            .filter(|glyph| !face.has_glyph(*glyph as u32))
            .collect()
    }

    /// Whether `face` supplies every required glyph.
    #[must_use]
    pub fn is_complete(self, face: &FontFace) -> bool {
        self.missing(face).is_empty()
    }

    /// The modes to test, widest first.
    #[must_use]
    pub const fn modes() -> [SubcellGrid; 3] {
        [
            SubcellGrid::Sextant,
            SubcellGrid::Quadrant,
            SubcellGrid::Half,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::GlyphCoverage;
    use crate::subcell_grid::SubcellGrid;

    #[test]
    fn half_blocks_need_three_glyphs() {
        assert_eq!(GlyphCoverage::new(SubcellGrid::Half).required().len(), 3);
    }

    #[test]
    fn quadrants_need_fifteen_glyphs() {
        assert_eq!(
            GlyphCoverage::new(SubcellGrid::Quadrant).required().len(),
            15
        );
    }

    #[test]
    fn sextants_need_sixty_three_glyphs() {
        assert_eq!(
            GlyphCoverage::new(SubcellGrid::Sextant).required().len(),
            63
        );
    }

    #[test]
    fn the_blank_glyph_is_never_required() {
        for mode in GlyphCoverage::modes() {
            let required = GlyphCoverage::new(mode).required();
            assert!(!required.contains(&' '), "{}", mode.name());
        }
    }

    #[test]
    fn sextant_coverage_includes_the_borrowed_half_blocks() {
        let required = GlyphCoverage::new(SubcellGrid::Sextant).required();
        assert!(required.contains(&'\u{258C}'));
        assert!(required.contains(&'\u{2590}'));
        assert!(required.contains(&'\u{2588}'));
    }

    #[test]
    fn most_sextant_glyphs_come_from_the_legacy_computing_block() {
        let required = GlyphCoverage::new(SubcellGrid::Sextant).required();
        let legacy = required
            .iter()
            .filter(|glyph| ('\u{1FB00}'..='\u{1FB3B}').contains(*glyph))
            .count();
        assert_eq!(legacy, 60);
    }
}
