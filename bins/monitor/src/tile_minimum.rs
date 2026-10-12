//! [`TileMinimum`] -- the smallest cell a process tile can have and stay
//! readable, and the floor for the band of fixed tiles above them.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileMinimum {
    pub width: i32,
    pub height: i32,
    pub band: i32,
}

impl TileMinimum {
    /// Cells at least `side` square, with the band at least three quarters
    /// of that.
    #[must_use]
    pub const fn square(side: i32) -> Self {
        Self {
            width: side,
            height: side,
            band: side * 3 / 4,
        }
    }

    /// Width over height; the shape the layout aims its cells at.
    #[must_use]
    pub fn aspect(&self) -> f64 {
        if self.height <= 0 {
            return 1.0;
        }
        f64::from(self.width) / f64::from(self.height)
    }

    #[must_use]
    pub const fn admits(&self, width: i32, height: i32) -> bool {
        width >= self.width && height >= self.height
    }
}

#[cfg(test)]
mod tests {
    use super::TileMinimum;

    #[test]
    fn a_cell_must_clear_both_sides() {
        let minimum = TileMinimum {
            width: 100,
            height: 40,
            band: 60,
        };
        assert!(minimum.admits(100, 40));
        assert!(!minimum.admits(99, 400));
        assert!(!minimum.admits(400, 39));
    }

    #[test]
    fn a_square_minimum_keeps_the_band_shorter_than_a_cell() {
        let minimum = TileMinimum::square(112);
        assert_eq!(
            (minimum.width, minimum.height, minimum.band),
            (112, 112, 84)
        );
        assert!((minimum.aspect() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_flat_minimum_has_no_aspect_of_its_own() {
        assert!((TileMinimum::square(0).aspect() - 1.0).abs() < 1e-9);
    }
}
