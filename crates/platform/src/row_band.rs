//! Maps a per-row stale bitmap onto the scanline ranges of a frame.

use std::ops::Range;

/// A `width` × `height` frame split into rows `row_h` scanlines tall.
///
/// The last row always reaches the frame bottom, so a height that is not a
/// multiple of `row_h` leaves no orphan strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowBand {
    width: usize,
    height: usize,
    row_h: usize,
}

impl RowBand {
    #[must_use]
    pub fn new(width: i32, height: i32, row_h: i32) -> Self {
        Self {
            width: usize::try_from(width).unwrap_or(0),
            height: usize::try_from(height).unwrap_or(0),
            row_h: usize::try_from(row_h).unwrap_or(1).max(1),
        }
    }

    /// A band whose single row is the whole frame, for windows that repaint
    /// everything each present.
    #[must_use]
    pub fn whole(width: i32, height: i32) -> Self {
        Self::new(width, height, height)
    }

    #[must_use]
    pub const fn width(self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> usize {
        self.height
    }

    #[must_use]
    pub const fn row_h(self) -> usize {
        self.row_h
    }

    /// Rows needed to cover the frame, counting a partial last row.
    #[must_use]
    pub const fn rows(self) -> usize {
        let rows = self.height.div_ceil(self.row_h);
        if rows == 0 { 1 } else { rows }
    }

    /// Scanline ranges covering every run of consecutive `true` rows.
    #[must_use]
    pub const fn runs(self, stale: &[bool]) -> StaleRuns<'_> {
        StaleRuns {
            stale,
            band: self,
            row: 0,
        }
    }

    /// The pixel index range of scanlines `lines` in a row-major frame.
    #[must_use]
    pub const fn pixels(self, lines: &Range<usize>) -> Range<usize> {
        lines.start * self.width..lines.end * self.width
    }
}

/// Iterator returned by [`RowBand::runs`].
pub struct StaleRuns<'a> {
    stale: &'a [bool],
    band: RowBand,
    row: usize,
}

impl Iterator for StaleRuns<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        loop {
            while self.stale.get(self.row) == Some(&false) {
                self.row += 1;
            }
            if self.row >= self.stale.len() {
                return None;
            }
            let start = self.row;
            while self.stale.get(self.row) == Some(&true) {
                self.row += 1;
            }
            let y0 = (start * self.band.row_h).min(self.band.height);
            let y1 = if self.row == self.stale.len() {
                self.band.height
            } else {
                (self.row * self.band.row_h).min(self.band.height)
            };
            if y0 < y1 {
                return Some(y0..y1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_rows_coalesce_into_one_run() {
        let band = RowBand::new(4, 40, 10);
        let runs: Vec<_> = band.runs(&[false, true, true, false]).collect();
        assert_eq!(runs, vec![10..30]);
    }

    #[test]
    fn separate_runs_stay_separate() {
        let band = RowBand::new(4, 40, 10);
        let runs: Vec<_> = band.runs(&[true, false, false, true]).collect();
        assert_eq!(runs, vec![0..10, 30..40]);
    }

    #[test]
    fn a_run_through_the_last_row_reaches_the_frame_bottom() {
        let band = RowBand::new(4, 37, 10);
        let runs: Vec<_> = band.runs(&[false, false, false, true]).collect();
        assert_eq!(runs, vec![30..37]);
    }

    #[test]
    fn rows_past_the_frame_yield_nothing() {
        let band = RowBand::new(4, 10, 10);
        let runs: Vec<_> = band.runs(&[false, false, true]).collect();
        assert_eq!(runs, vec![]);
    }

    #[test]
    fn clean_bitmap_yields_nothing() {
        let band = RowBand::new(4, 40, 10);
        assert_eq!(band.runs(&[false; 4]).count(), 0);
    }

    #[test]
    fn rows_count_a_partial_last_row() {
        assert_eq!(RowBand::new(4, 37, 10).rows(), 4);
        assert_eq!(RowBand::new(4, 40, 10).rows(), 4);
        assert_eq!(RowBand::whole(4, 40).rows(), 1);
    }

    #[test]
    fn pixels_scale_scanlines_by_width() {
        let band = RowBand::new(4, 40, 10);
        assert_eq!(band.pixels(&(10..30)), 40..120);
    }
}
