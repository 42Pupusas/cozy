//! The subdivision of one terminal cell into addressable blocks.
//!
//! A cell is one glyph, but Unicode block characters divide that glyph into
//! parts, so a grid of C by R cells can show more than C by R samples. The
//! choice trades resolution against font coverage:
//!
//! * Half blocks use `U+2580`..`U+2588`, present in almost every font.
//! * Quadrants use `U+2596`..`U+259F`, present in most fonts.
//! * Sextants use `U+1FB00`..`U+1FB3B` from Symbols for Legacy Computing,
//!   which needs a recent font but triples the vertical resolution.

/// First sextant codepoint, which encodes the mask `0b000001`.
const SEXTANT_BASE: u32 = 0x1FB00;

/// Sextant masks that Unicode encodes as older block characters instead.
const SEXTANT_LEFT_COLUMN: u8 = 0b01_0101;
const SEXTANT_RIGHT_COLUMN: u8 = 0b10_1010;

/// The 16 quadrant glyphs, indexed by a mask of set subcells.
const QUADRANTS: [char; 16] = [
    ' ',
    '\u{2598}',
    '\u{259D}',
    '\u{2580}',
    '\u{2596}',
    '\u{258C}',
    '\u{259E}',
    '\u{259B}',
    '\u{2597}',
    '\u{259A}',
    '\u{2590}',
    '\u{259C}',
    '\u{2584}',
    '\u{2599}',
    '\u{259F}',
    '\u{2588}',
];

/// The four half-block glyphs, indexed by a mask of set subcells.
const HALVES: [char; 4] = [' ', '\u{2580}', '\u{2584}', '\u{2588}'];

/// How one cell divides into independently lit blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubcellGrid {
    /// One column by two rows per cell.
    #[default]
    Half,
    /// Two columns by two rows per cell.
    Quadrant,
    /// Two columns by three rows per cell.
    Sextant,
}

impl SubcellGrid {
    /// Parse the name used on the command line.
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "half" => Ok(Self::Half),
            "quadrant" => Ok(Self::Quadrant),
            "sextant" => Ok(Self::Sextant),
            other => Err(format!(
                "unknown cell mode: {other} (expected half, quadrant, or sextant)"
            )),
        }
    }

    /// The name used on the command line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Half => "half",
            Self::Quadrant => "quadrant",
            Self::Sextant => "sextant",
        }
    }

    /// Subcell columns in one cell.
    #[must_use]
    pub const fn columns(self) -> u32 {
        match self {
            Self::Half => 1,
            Self::Quadrant | Self::Sextant => 2,
        }
    }

    /// Subcell rows in one cell.
    #[must_use]
    pub const fn rows(self) -> u32 {
        match self {
            Self::Half | Self::Quadrant => 2,
            Self::Sextant => 3,
        }
    }

    /// Subcells in one cell.
    #[must_use]
    pub const fn count(self) -> u32 {
        self.columns() * self.rows()
    }

    /// The mask value of the subcell at `(column, row)` inside a cell.
    ///
    /// Bit order runs left to right, then top to bottom, which matches the
    /// order Unicode assigns to both quadrants and sextants.
    #[must_use]
    pub const fn bit(self, column: u32, row: u32) -> u8 {
        1u8 << (row * self.columns() + column)
    }

    /// The glyph that lights exactly the subcells named by `mask`.
    #[must_use]
    pub const fn glyph(self, mask: u8) -> char {
        match self {
            Self::Half => HALVES[(mask & 0b11) as usize],
            Self::Quadrant => QUADRANTS[(mask & 0b1111) as usize],
            Self::Sextant => Self::sextant(mask & 0b0011_1111),
        }
    }

    /// The sextant glyph for a six-bit mask.
    ///
    /// Unicode omits four masks from the sextant block because older
    /// characters already cover them, so the index shifts past each omission.
    const fn sextant(mask: u8) -> char {
        match mask {
            0 => ' ',
            0b0011_1111 => '\u{2588}',
            SEXTANT_LEFT_COLUMN => '\u{258C}',
            SEXTANT_RIGHT_COLUMN => '\u{2590}',
            _ => {
                let mut index = mask as u32 - 1;
                if mask > SEXTANT_LEFT_COLUMN {
                    index -= 1;
                }
                if mask > SEXTANT_RIGHT_COLUMN {
                    index -= 1;
                }
                match char::from_u32(SEXTANT_BASE + index) {
                    Some(glyph) => glyph,
                    None => '\u{2588}',
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SubcellGrid;

    #[test]
    fn each_mode_reports_its_subcell_shape() {
        assert_eq!((SubcellGrid::Half.columns(), SubcellGrid::Half.rows()), (1, 2));
        assert_eq!(
            (SubcellGrid::Quadrant.columns(), SubcellGrid::Quadrant.rows()),
            (2, 2)
        );
        assert_eq!(
            (SubcellGrid::Sextant.columns(), SubcellGrid::Sextant.rows()),
            (2, 3)
        );
        assert_eq!(SubcellGrid::Sextant.count(), 6);
    }

    #[test]
    fn an_empty_mask_is_a_space_in_every_mode() {
        for mode in [
            SubcellGrid::Half,
            SubcellGrid::Quadrant,
            SubcellGrid::Sextant,
        ] {
            assert_eq!(mode.glyph(0), ' ', "{}", mode.name());
        }
    }

    #[test]
    fn a_full_mask_is_a_full_block_in_every_mode() {
        assert_eq!(SubcellGrid::Half.glyph(0b11), '\u{2588}');
        assert_eq!(SubcellGrid::Quadrant.glyph(0b1111), '\u{2588}');
        assert_eq!(SubcellGrid::Sextant.glyph(0b0011_1111), '\u{2588}');
    }

    #[test]
    fn bits_run_left_to_right_then_top_to_bottom() {
        let sextant = SubcellGrid::Sextant;
        assert_eq!(sextant.bit(0, 0), 0b00_0001);
        assert_eq!(sextant.bit(1, 0), 0b00_0010);
        assert_eq!(sextant.bit(0, 2), 0b01_0000);
        assert_eq!(sextant.bit(1, 2), 0b10_0000);
    }

    #[test]
    fn quadrant_masks_match_their_unicode_names() {
        let quadrant = SubcellGrid::Quadrant;
        assert_eq!(quadrant.glyph(quadrant.bit(0, 0)), '\u{2598}');
        assert_eq!(quadrant.glyph(quadrant.bit(1, 0)), '\u{259D}');
        assert_eq!(quadrant.glyph(quadrant.bit(0, 1)), '\u{2596}');
        assert_eq!(quadrant.glyph(quadrant.bit(1, 1)), '\u{2597}');
        assert_eq!(quadrant.glyph(0b0011), '\u{2580}');
        assert_eq!(quadrant.glyph(0b1100), '\u{2584}');
        assert_eq!(quadrant.glyph(0b0101), '\u{258C}');
        assert_eq!(quadrant.glyph(0b1010), '\u{2590}');
    }

    #[test]
    fn the_sextant_block_starts_at_its_first_codepoint() {
        assert_eq!(SubcellGrid::Sextant.glyph(0b00_0001), '\u{1FB00}');
        assert_eq!(SubcellGrid::Sextant.glyph(0b11_1110), '\u{1FB3B}');
    }

    #[test]
    fn sextant_columns_use_the_older_half_block_characters() {
        assert_eq!(SubcellGrid::Sextant.glyph(0b01_0101), '\u{258C}');
        assert_eq!(SubcellGrid::Sextant.glyph(0b10_1010), '\u{2590}');
    }

    #[test]
    fn every_sextant_mask_maps_to_a_distinct_glyph() {
        let mut seen: Vec<char> = (0..64u8).map(|mask| SubcellGrid::Sextant.glyph(mask)).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), 64);
    }

    #[test]
    fn mode_names_round_trip() {
        for mode in [
            SubcellGrid::Half,
            SubcellGrid::Quadrant,
            SubcellGrid::Sextant,
        ] {
            assert_eq!(SubcellGrid::parse(mode.name()), Ok(mode));
        }
        assert!(SubcellGrid::parse("octant").is_err());
    }
}
