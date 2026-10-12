//! Coalesces a per-row stale bitmap into scanline ranges of a frame.

use std::ops::Range;

use crate::row_layout::RowLayout;

/// Scanline ranges covering every run of consecutive `true` rows in `stale`,
/// placed by `layout`. Flags past the layout's last row are ignored.
pub struct StaleRuns<'a> {
    stale: &'a [bool],
    layout: RowLayout,
    row: usize,
}

impl<'a> StaleRuns<'a> {
    #[must_use]
    pub const fn new(layout: RowLayout, stale: &'a [bool]) -> Self {
        Self {
            stale,
            layout,
            row: 0,
        }
    }
}

impl Iterator for StaleRuns<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let rows = self.stale.len().min(self.layout.rows());
        while self.row < rows && !self.stale[self.row] {
            self.row += 1;
        }
        if self.row >= rows {
            return None;
        }
        let start = self.row;
        while self.row < rows && self.stale[self.row] {
            self.row += 1;
        }
        Some(self.layout.boundary(start)..self.layout.boundary(self.row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(layout: RowLayout, stale: &[bool]) -> Vec<Range<usize>> {
        StaleRuns::new(layout, stale).collect()
    }

    #[test]
    fn consecutive_rows_coalesce_into_one_run() {
        let layout = RowLayout::new(4, 40, 10);
        assert_eq!(runs(layout, &[false, true, true, false]), vec![10..30]);
    }

    #[test]
    fn separate_runs_stay_separate() {
        let layout = RowLayout::new(4, 40, 10);
        assert_eq!(runs(layout, &[true, false, false, true]), vec![0..10, 30..40]);
    }

    #[test]
    fn a_run_through_the_last_row_reaches_the_frame_bottom() {
        let layout = RowLayout::new(4, 37, 10);
        assert_eq!(runs(layout, &[false, false, true]), vec![layout.boundary(2)..37]);
    }

    #[test]
    fn flags_past_the_last_row_yield_nothing() {
        let layout = RowLayout::new(4, 10, 10);
        assert_eq!(runs(layout, &[false, true, true]), vec![]);
    }

    #[test]
    fn a_clean_bitmap_yields_nothing() {
        let layout = RowLayout::new(4, 40, 10);
        assert_eq!(runs(layout, &[false; 4]), vec![]);
    }
}
