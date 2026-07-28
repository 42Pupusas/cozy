//! Scroll geometry for a uniform-height list.
//!
//! This is the piece that makes a 100k-entry directory cheap: nothing here
//! ever touches the items themselves. The viewport knows only the row height,
//! how many rows exist, and how tall the visible area is -- from which it
//! derives *which slice of indices is on screen*. The caller then paints only
//! that slice, so painting cost is proportional to the window, not to the
//! directory.
//!
//! Kept deliberately free of any drawing or Wayland types so the awkward
//! cases (empty list, list shorter than the viewport, a resize that strands
//! the offset past the end) are unit-testable in microseconds instead of
//! needing a compositor and a squint.
//!
//! ### Why pixels rather than a row index
//! The scroll position is a **pixel** offset, not a first-visible-row index.
//! A row-index offset can only ever show rows aligned to the top edge, which
//! makes a scroll wheel feel notchy and makes a partially-revealed bottom row
//! impossible. Pixels let a row be half-scrolled off the top, which is what
//! every real list does.

/// A window onto a list of uniform-height rows.
///
/// All lengths are in pixels except `len`, which counts rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListViewport {
    /// Height of a single row in pixels. Always at least 1 -- a zero row
    /// height would make "how many rows fit" a division by zero, and every
    /// call site would need its own guard.
    row_height: i32,
    /// Number of rows in the backing list.
    len: usize,
    /// Height of the visible area in pixels. May be zero (a window collapsed
    /// to nothing), which simply means nothing is visible.
    height: i32,
    /// Distance in pixels from the top of the content to the top of the
    /// visible area. Always within `0..=max_scroll()`.
    offset: i32,
}

impl ListViewport {
    /// Builds a viewport. `row_height` is clamped to at least 1 and `height`
    /// to at least 0, so a caller passing a degenerate configure size gets a
    /// viewport that shows nothing rather than one that panics.
    #[must_use]
    pub fn new(row_height: i32, len: usize, height: i32) -> Self {
        Self {
            row_height: row_height.max(1),
            len,
            height: height.max(0),
            offset: 0,
        }
    }

    /// Total height of all rows stacked up.
    ///
    /// Computed in `i64` and saturated back down: 100k rows is harmless, but
    /// this is the one product in the module that a pathological `len` could
    /// overflow, and a wrapped negative content height would invert every
    /// clamp below it.
    #[must_use]
    pub fn content_height(&self) -> i32 {
        // `saturating_mul`, not `*`: `usize::MAX as i64 * row_height`
        // overflows i64 as well, so widening alone is not the guard -- the
        // multiply itself has to saturate.
        let total = i64::try_from(self.len)
            .unwrap_or(i64::MAX)
            .saturating_mul(i64::from(self.row_height));
        i32::try_from(total.min(i64::from(i32::MAX))).unwrap_or(i32::MAX)
    }

    /// The largest legal scroll offset: the point where the last row sits
    /// flush with the bottom edge.
    ///
    /// Zero when the content is shorter than the viewport, which is what
    /// pins a short list to the top instead of letting it drift.
    #[must_use]
    pub fn max_scroll(&self) -> i32 {
        (self.content_height() - self.height).max(0)
    }

    /// The current scroll offset in pixels.
    #[must_use]
    pub const fn offset(&self) -> i32 {
        self.offset
    }

    /// Number of rows in the list.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the list has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Height of one row in pixels.
    #[must_use]
    pub const fn row_height(&self) -> i32 {
        self.row_height
    }

    /// Height of the visible area in pixels.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.height
    }

    /// Forces the offset back into `0..=max_scroll()`.
    ///
    /// Called after anything that can shrink the content or grow the
    /// viewport. Without it, deleting most of a directory -- or resizing the
    /// window taller -- would leave the offset pointing past the end and the
    /// list would render as a blank expanse below the final row.
    fn clamp_offset(&mut self) {
        self.offset = self.offset.clamp(0, self.max_scroll());
    }

    /// Replaces the row count, keeping the scroll position where it is if
    /// that is still legal.
    ///
    /// Preserving the offset matters for a *refresh*: a file appearing in a
    /// watched directory should not fling the user back to the top.
    pub fn set_len(&mut self, len: usize) {
        self.len = len;
        self.clamp_offset();
    }

    /// Replaces the viewport height, e.g. on a window resize.
    pub fn set_height(&mut self, height: i32) {
        self.height = height.max(0);
        self.clamp_offset();
    }

    /// Replaces the row height, e.g. when the font size changes.
    pub fn set_row_height(&mut self, row_height: i32) {
        self.row_height = row_height.max(1);
        self.clamp_offset();
    }

    /// Scrolls by a pixel delta (positive scrolls down), clamping at both
    /// ends. Returns whether the offset actually moved, so a caller can skip
    /// requesting a repaint when the list is already hard against a stop.
    pub fn scroll_by(&mut self, delta: i32) -> bool {
        let before = self.offset;
        self.offset = self
            .offset
            .saturating_add(delta)
            .clamp(0, self.max_scroll());
        self.offset != before
    }

    /// Scrolls to an absolute pixel offset, clamped. Returns whether it moved.
    pub fn scroll_to(&mut self, offset: i32) -> bool {
        let before = self.offset;
        self.offset = offset.clamp(0, self.max_scroll());
        self.offset != before
    }

    /// Scrolls by whole viewport-fuls (Page Up / Page Down).
    ///
    /// Overlaps by one row so the line at the boundary stays on screen
    /// across the jump -- losing your place at every page break is the
    /// classic paging annoyance. Degrades to a single row when the viewport
    /// is too short for the overlap to make sense.
    pub fn scroll_pages(&mut self, pages: i32) -> bool {
        let step = (self.height - self.row_height).max(self.row_height);
        self.scroll_by(step.saturating_mul(pages))
    }

    /// The half-open range of row indices that are at least partly visible.
    ///
    /// The first index is the row containing the top edge; the end is one
    /// past the row containing the bottom edge, so a row peeking in by a
    /// single pixel is included -- omitting it would leave a visible gap at
    /// the bottom of the window.
    #[must_use]
    pub fn visible_rows(&self) -> core::ops::Range<usize> {
        if self.len == 0 || self.height <= 0 {
            return 0..0;
        }
        let first = (self.offset / self.row_height).max(0);
        // Round the bottom edge up so a partially-shown row still counts.
        // Done unsigned because `i32::div_ceil` is still unstable; both
        // operands are non-negative here (offset and height are clamped to
        // >= 0, row_height to >= 1), so the conversion is lossless.
        let last_px = self.offset.saturating_add(self.height);
        let last = i64::from(
            u32::try_from(last_px)
                .unwrap_or(0)
                .div_ceil(self.row_height.unsigned_abs()),
        );
        let first = usize::try_from(first).unwrap_or(0).min(self.len);
        let last = usize::try_from(last).unwrap_or(self.len).min(self.len);
        first..last
    }

    /// The y coordinate at which `row` should be painted, relative to the top
    /// of the viewport. Negative when the row is scrolled off the top.
    ///
    /// Returns `None` only for an out-of-range index, so callers can paint
    /// straight from a row number without a separate bounds check.
    #[must_use]
    pub fn row_y(&self, row: usize) -> Option<i32> {
        if row >= self.len {
            return None;
        }
        let top =
            i64::try_from(row).unwrap_or(0) * i64::from(self.row_height) - i64::from(self.offset);
        i32::try_from(top).ok()
    }

    /// The row index at viewport-relative y, if any.
    ///
    /// This is the hit test behind clicks and hover. Returns `None` above the
    /// list, below the last row, or when y falls in the empty space of a
    /// viewport taller than its content -- clicking that dead area must not
    /// select the final row.
    #[must_use]
    pub fn row_at_y(&self, y: i32) -> Option<usize> {
        if y < 0 || y >= self.height {
            return None;
        }
        let absolute = i64::from(self.offset) + i64::from(y);
        let row = absolute / i64::from(self.row_height);
        let row = usize::try_from(row).ok()?;
        (row < self.len).then_some(row)
    }

    /// Scrolls the minimum distance needed to bring `row` fully into view.
    ///
    /// Minimum distance is the point: this backs arrow-key navigation, where
    /// scrolling further than necessary (e.g. centring the row) makes the
    /// list lurch on every keypress. A row already fully visible does not
    /// move the offset at all.
    pub fn scroll_into_view(&mut self, row: usize) -> bool {
        if row >= self.len {
            return false;
        }
        let top = i32::try_from(i64::try_from(row).unwrap_or(0) * i64::from(self.row_height))
            .unwrap_or(i32::MAX);
        let bottom = top.saturating_add(self.row_height);
        if top < self.offset {
            // Off the top: align the row's top edge with the viewport's.
            self.scroll_to(top)
        } else if bottom > self.offset.saturating_add(self.height) {
            // Off the bottom: align the row's bottom edge with the viewport's.
            self.scroll_to(bottom.saturating_sub(self.height))
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ListViewport;

    /// 10 rows of 20px in a 100px window: 5 rows fit, 200px of content.
    fn viewport() -> ListViewport {
        ListViewport::new(20, 10, 100)
    }

    #[test]
    fn a_fresh_viewport_starts_at_the_top() {
        let v = viewport();
        assert_eq!(v.offset(), 0);
        assert_eq!(v.visible_rows(), 0..5);
    }

    #[test]
    fn content_height_is_rows_times_row_height() {
        assert_eq!(viewport().content_height(), 200);
    }

    #[test]
    fn max_scroll_leaves_the_last_row_flush_with_the_bottom() {
        let mut v = viewport();
        assert_eq!(v.max_scroll(), 100);
        v.scroll_to(100);
        // The final row's bottom edge sits exactly on the viewport's.
        assert_eq!(v.row_y(9), Some(80));
        assert_eq!(v.visible_rows(), 5..10);
    }

    #[test]
    fn a_list_shorter_than_the_viewport_cannot_scroll() {
        let mut v = ListViewport::new(20, 2, 100);
        assert_eq!(v.max_scroll(), 0);
        assert!(!v.scroll_by(50), "short list must stay pinned to the top");
        assert_eq!(v.offset(), 0);
        assert_eq!(v.visible_rows(), 0..2);
    }

    #[test]
    fn an_empty_list_has_no_visible_rows_and_no_hits() {
        let v = ListViewport::new(20, 0, 100);
        assert_eq!(v.visible_rows(), 0..0);
        assert_eq!(v.row_at_y(0), None);
        assert_eq!(v.row_y(0), None);
    }

    #[test]
    fn a_zero_height_viewport_shows_nothing() {
        let v = ListViewport::new(20, 10, 0);
        assert_eq!(v.visible_rows(), 0..0);
        assert_eq!(v.row_at_y(0), None);
    }

    #[test]
    fn row_height_is_clamped_away_from_zero() {
        // A zero row height would be a division by zero in visible_rows.
        let v = ListViewport::new(0, 10, 100);
        assert_eq!(v.row_height(), 1);
        assert_eq!(v.visible_rows(), 0..10);
    }

    #[test]
    fn scrolling_clamps_at_both_ends() {
        let mut v = viewport();
        assert!(!v.scroll_by(-10), "already at the top");
        assert_eq!(v.offset(), 0);
        assert!(v.scroll_by(10_000));
        assert_eq!(v.offset(), 100);
        assert!(!v.scroll_by(10_000), "already at the bottom");
    }

    #[test]
    fn scroll_by_reports_whether_it_moved() {
        let mut v = viewport();
        assert!(v.scroll_by(10));
        assert!(!v.scroll_by(0));
    }

    #[test]
    fn a_partially_scrolled_row_is_still_visible() {
        let mut v = viewport();
        v.scroll_to(10); // half of row 0 remains on screen
        assert_eq!(v.visible_rows(), 0..6, "both edge rows count as visible");
        assert_eq!(v.row_y(0), Some(-10));
    }

    #[test]
    fn row_y_is_negative_once_a_row_is_off_the_top() {
        let mut v = viewport();
        v.scroll_to(50);
        assert_eq!(v.row_y(0), Some(-50));
        assert_eq!(v.row_y(2), Some(-10));
        assert_eq!(v.row_y(3), Some(10));
    }

    #[test]
    fn row_y_rejects_an_out_of_range_index() {
        assert_eq!(viewport().row_y(10), None);
    }

    #[test]
    fn hit_testing_maps_y_back_to_the_row_under_it() {
        let v = viewport();
        assert_eq!(v.row_at_y(0), Some(0));
        assert_eq!(v.row_at_y(19), Some(0));
        assert_eq!(v.row_at_y(20), Some(1));
        assert_eq!(v.row_at_y(99), Some(4));
    }

    #[test]
    fn hit_testing_accounts_for_the_scroll_offset() {
        let mut v = viewport();
        v.scroll_to(100);
        assert_eq!(v.row_at_y(0), Some(5));
        assert_eq!(v.row_at_y(99), Some(9));
    }

    #[test]
    fn hit_testing_outside_the_viewport_is_a_miss() {
        let v = viewport();
        assert_eq!(v.row_at_y(-1), None);
        assert_eq!(v.row_at_y(100), None);
    }

    #[test]
    fn clicking_empty_space_below_a_short_list_selects_nothing() {
        // 2 rows of 20px in a 100px window: y=50 is past the content.
        let v = ListViewport::new(20, 2, 100);
        assert_eq!(v.row_at_y(39), Some(1));
        assert_eq!(v.row_at_y(40), None, "dead space must not hit the last row");
    }

    #[test]
    fn hit_testing_round_trips_with_row_y() {
        let mut v = viewport();
        v.scroll_to(37); // deliberately not row-aligned
        for row in v.visible_rows() {
            let y = v.row_y(row).expect("visible row has a y");
            // Probe inside the row's band, skipping any part off-screen.
            let probe = y.max(0);
            if probe < v.height() && y + v.row_height() > 0 {
                assert_eq!(v.row_at_y(probe), Some(row), "row {row} at y={probe}");
            }
        }
    }

    #[test]
    fn growing_the_list_keeps_the_scroll_position() {
        let mut v = viewport();
        v.scroll_to(60);
        v.set_len(1000);
        assert_eq!(
            v.offset(),
            60,
            "a refresh must not fling the user to the top"
        );
    }

    #[test]
    fn shrinking_the_list_pulls_a_stranded_offset_back() {
        let mut v = viewport();
        v.scroll_to(100);
        v.set_len(3); // content is now 60px, shorter than the 100px viewport
        assert_eq!(v.offset(), 0, "offset must not point past the end");
        assert_eq!(v.visible_rows(), 0..3);
    }

    #[test]
    fn growing_the_viewport_pulls_the_offset_back() {
        let mut v = viewport();
        v.scroll_to(100);
        v.set_height(200); // now taller than the 200px content
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn changing_row_height_reclamps() {
        let mut v = viewport();
        v.scroll_to(100);
        v.set_row_height(5); // content shrinks to 50px
        assert_eq!(v.offset(), 0);
        assert_eq!(v.row_height(), 5);
    }

    #[test]
    fn paging_overlaps_by_one_row_so_the_boundary_line_is_not_lost() {
        let mut v = viewport();
        v.scroll_pages(1);
        // 100px viewport - 20px row = 80px step, not a full 100px.
        assert_eq!(v.offset(), 80);
        let rows = v.visible_rows();
        assert!(
            rows.contains(&4),
            "the row at the old bottom edge is still shown"
        );
    }

    #[test]
    fn paging_back_up_clamps_at_the_top() {
        let mut v = viewport();
        v.scroll_pages(-1);
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn paging_in_a_viewport_shorter_than_a_row_still_advances() {
        // Viewport smaller than one row would give a zero or negative step,
        // which would make Page Down do nothing at all.
        let mut v = ListViewport::new(20, 100, 10);
        assert!(v.scroll_pages(1));
        assert_eq!(v.offset(), 20);
    }

    #[test]
    fn scroll_into_view_leaves_an_already_visible_row_alone() {
        let mut v = viewport();
        assert!(!v.scroll_into_view(3), "row 3 is already fully on screen");
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn scroll_into_view_reveals_a_row_below_by_the_minimum_distance() {
        let mut v = viewport();
        assert!(v.scroll_into_view(5));
        // Row 5 spans 100..120, so the bottom edge lands at exactly 120.
        assert_eq!(v.offset(), 20, "must not overshoot past the minimum");
        assert_eq!(v.row_y(5), Some(80));
    }

    #[test]
    fn scroll_into_view_reveals_a_row_above() {
        let mut v = viewport();
        v.scroll_to(100);
        assert!(v.scroll_into_view(2));
        assert_eq!(v.offset(), 40, "row 2's top edge aligns with the viewport");
        assert_eq!(v.row_y(2), Some(0));
    }

    #[test]
    fn scroll_into_view_ignores_an_out_of_range_row() {
        let mut v = viewport();
        assert!(!v.scroll_into_view(99));
        assert_eq!(v.offset(), 0);
    }

    #[test]
    fn walking_every_row_with_scroll_into_view_keeps_it_visible() {
        // Simulates holding Down from the top: every row must end up fully
        // on screen, and the offset must never leave its legal range.
        let mut v = ListViewport::new(20, 50, 100);
        for row in 0..50 {
            v.scroll_into_view(row);
            assert!(v.offset() >= 0 && v.offset() <= v.max_scroll());
            let y = v.row_y(row).expect("row exists");
            assert!(y >= 0, "row {row} is off the top (y={y})");
            assert!(
                y + v.row_height() <= v.height(),
                "row {row} is off the bottom (y={y})"
            );
        }
    }

    #[test]
    fn a_hundred_thousand_rows_only_ever_yields_a_screenful() {
        // The whole point of virtualisation: cost tracks the window, not the
        // directory. A 100k-entry listing must still paint ~26 rows.
        let mut v = ListViewport::new(24, 100_000, 600);
        assert_eq!(v.content_height(), 2_400_000);
        assert!(v.visible_rows().len() <= 26);
        v.scroll_to(v.max_scroll());
        assert!(v.visible_rows().len() <= 26);
        assert_eq!(v.visible_rows().end, 100_000);
    }

    #[test]
    fn a_pathological_row_count_saturates_instead_of_overflowing() {
        // usize::MAX rows * any height overflows i32 and even i64. A wrapped
        // negative content height would invert every clamp in the module.
        let v = ListViewport::new(1000, usize::MAX, 100);
        assert_eq!(v.content_height(), i32::MAX);
        assert!(v.max_scroll() > 0);
    }
}
