//! An ordered threshold matrix for reducing coverage to two values.
//!
//! Area sampling produces a coverage fraction per subcell, but a block glyph
//! can only light a subcell or leave it dark. Comparing every subcell against
//! the same threshold turns a smooth gradient into a hard edge. A Bayer
//! matrix varies the threshold by position instead, so a half-covered region
//! becomes a checker of lit and dark subcells that reads as grey.

/// Edge length of the threshold matrix.
const ORDER: usize = 4;

/// Coverage step between two adjacent ranks.
///
/// Sixteen ranks times sixteen steps reaches 240, which stays inside a `u8`
/// and leaves full coverage strictly above the highest threshold.
const RANK_STEP: u8 = 16;

/// The 4x4 Bayer matrix, in its conventional 0..15 ranking.
const RANKS: [[u8; ORDER]; ORDER] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
];

/// Position-dependent thresholds spread evenly across the coverage range.
pub struct DitherMatrix {
    thresholds: [[u8; ORDER]; ORDER],
}

impl Default for DitherMatrix {
    fn default() -> Self {
        Self::new()
    }
}

impl DitherMatrix {
    /// Build the matrix, mapping each rank onto a distinct threshold.
    #[must_use]
    pub const fn new() -> Self {
        let mut thresholds = [[0u8; ORDER]; ORDER];
        let mut row = 0usize;
        while row < ORDER {
            let mut column: usize = 0;
            while column < ORDER {
                thresholds[row][column] = RANKS[row][column] * RANK_STEP;
                column += 1;
            }
            row += 1;
        }
        Self { thresholds }
    }

    /// The threshold applied at `(x, y)`.
    #[must_use]
    pub const fn threshold(&self, x: u32, y: u32) -> u8 {
        self.thresholds[y as usize % ORDER][x as usize % ORDER]
    }

    /// Whether `coverage` lights the subcell at `(x, y)`.
    #[must_use]
    pub const fn is_lit(&self, x: u32, y: u32, coverage: u8) -> bool {
        coverage > self.threshold(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::DitherMatrix;

    #[test]
    fn thresholds_span_the_coverage_range() {
        let matrix = DitherMatrix::new();
        let mut seen: Vec<u8> = (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .map(|(x, y)| matrix.threshold(x, y))
            .collect();
        seen.sort_unstable();
        assert_eq!(seen.len(), 16);
        seen.dedup();
        assert_eq!(seen.len(), 16);
        assert_eq!(seen[0], 0);
        assert!(seen[15] < u8::MAX);
    }

    #[test]
    fn the_matrix_tiles_the_plane() {
        let matrix = DitherMatrix::new();
        assert_eq!(matrix.threshold(0, 0), matrix.threshold(4, 4));
        assert_eq!(matrix.threshold(1, 2), matrix.threshold(9, 6));
    }

    #[test]
    fn full_coverage_lights_every_position() {
        let matrix = DitherMatrix::new();
        for y in 0..4 {
            for x in 0..4 {
                assert!(matrix.is_lit(x, y, 255));
            }
        }
    }

    #[test]
    fn no_coverage_lights_nothing() {
        let matrix = DitherMatrix::new();
        for y in 0..4 {
            for x in 0..4 {
                assert!(!matrix.is_lit(x, y, 0));
            }
        }
    }

    #[test]
    fn half_coverage_lights_half_the_positions() {
        let matrix = DitherMatrix::new();
        let lit = (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .filter(|(x, y)| matrix.is_lit(*x, *y, 127))
            .count();
        assert_eq!(lit, 8);
    }

    #[test]
    fn rising_coverage_never_darkens_a_position() {
        let matrix = DitherMatrix::new();
        for y in 0..4 {
            for x in 0..4 {
                let first = (0..=u8::MAX)
                    .find(|coverage| matrix.is_lit(x, y, *coverage))
                    .unwrap();
                assert!((first..=u8::MAX).all(|coverage| matrix.is_lit(x, y, coverage)));
            }
        }
    }
}
