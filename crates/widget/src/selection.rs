//! Selection and cursor state for a list.
//!
//! Split from [`ListViewport`](crate::ListViewport) deliberately: *what is
//! selected* and *what is scrolled into view* are independent concerns that
//! happen to be used together. Keeping them apart means selection rules can
//! be tested without any geometry at all, and a viewport resize can never
//! accidentally perturb the selection.
//!
//! ### The cursor/anchor model
//! Two indices, as every file manager and text editor uses:
//!
//! - the **cursor** is the row with focus, the one arrow keys move and the
//!   one a plain Enter acts on;
//! - the **anchor** is where a range selection started.
//!
//! A plain click sets both. A shift-click moves only the cursor, and the
//! selection is the whole span between them -- which is what makes
//! shift-clicking twice *replace* the range rather than accumulate, since the
//! anchor stayed put. Ctrl-click toggles one row and re-anchors there.
//!
//! Selected rows are held as a sorted `Vec` rather than a `HashSet`: these
//! sets are small in practice, and a deterministic order means "copy the
//! selection" hands out paths in list order instead of hash order.

/// Which rows are selected, and where the keyboard cursor sits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSelection {
    /// Sorted, deduplicated row indices.
    selected: Vec<usize>,
    /// The focused row, if any. `None` before the first interaction.
    cursor: Option<usize>,
    /// Where the current range selection began.
    anchor: Option<usize>,
}

impl ListSelection {
    /// An empty selection with no cursor.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            selected: Vec::new(),
            cursor: None,
            anchor: None,
        }
    }

    /// The selected row indices, in ascending order.
    #[must_use]
    pub fn selected(&self) -> &[usize] {
        &self.selected
    }

    /// The focused row, if any.
    #[must_use]
    pub const fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    /// Whether `row` is selected.
    #[must_use]
    pub fn contains(&self, row: usize) -> bool {
        self.selected.binary_search(&row).is_ok()
    }

    /// How many rows are selected.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.selected.len()
    }

    /// Whether nothing is selected.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Clears the selection *and* the cursor -- e.g. on entering a new
    /// directory, where every previous index is meaningless.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
    }

    /// Selects exactly `row`, discarding any previous selection. The plain
    /// click / plain arrow-key case.
    pub fn select_only(&mut self, row: usize) {
        self.selected.clear();
        self.selected.push(row);
        self.cursor = Some(row);
        self.anchor = Some(row);
    }

    /// Toggles `row` and re-anchors there (Ctrl-click).
    ///
    /// Re-anchoring matters: a subsequent shift-click should extend from the
    /// row the user last touched, not from some older anchor.
    pub fn toggle(&mut self, row: usize) {
        match self.selected.binary_search(&row) {
            Ok(at) => {
                self.selected.remove(at);
            }
            Err(at) => self.selected.insert(at, row),
        }
        self.cursor = Some(row);
        self.anchor = Some(row);
    }

    /// Selects the inclusive span from the anchor to `row` (shift-click),
    /// replacing the previous selection but leaving the anchor alone.
    ///
    /// Falls back to a plain select when there is no anchor yet, so a
    /// shift-click into a fresh list does something sensible rather than
    /// nothing.
    pub fn extend_to(&mut self, row: usize) {
        let Some(anchor) = self.anchor else {
            self.select_only(row);
            return;
        };
        let (lo, hi) = if anchor <= row {
            (anchor, row)
        } else {
            (row, anchor)
        };
        self.selected.clear();
        self.selected.extend(lo..=hi);
        self.cursor = Some(row);
    }

    /// Selects every row in a list of `len` rows (Ctrl-A), leaving the cursor
    /// where it is.
    pub fn select_all(&mut self, len: usize) {
        self.selected.clear();
        self.selected.extend(0..len);
        if self.cursor.is_none() {
            self.cursor = Some(0);
            self.anchor = Some(0);
        }
    }

    /// Moves the cursor by `delta` rows within a list of `len` rows,
    /// selecting the row it lands on. Returns the new cursor.
    ///
    /// Clamps at both ends rather than wrapping: wrapping from the last row
    /// to the first on a held arrow key is disorienting in a file list.
    /// Starting from no cursor, a downward move lands on the first row and an
    /// upward move on the last, which is what makes the very first Down or Up
    /// keypress in a fresh window do the obvious thing.
    pub fn move_cursor(&mut self, delta: i32, len: usize) -> Option<usize> {
        if len == 0 {
            self.clear();
            return None;
        }
        let last = len - 1;
        let target = match self.cursor {
            Some(current) => {
                let current = i64::try_from(current).unwrap_or(0);
                let moved = current.saturating_add(i64::from(delta));
                let clamped = moved.clamp(0, i64::try_from(last).unwrap_or(i64::MAX));
                usize::try_from(clamped).unwrap_or(0)
            }
            None if delta < 0 => last,
            None => 0,
        };
        self.select_only(target);
        Some(target)
    }

    /// Moves the cursor by `delta` while extending the selection from the
    /// anchor (shift-arrow). Returns the new cursor.
    pub fn move_cursor_extending(&mut self, delta: i32, len: usize) -> Option<usize> {
        if len == 0 {
            self.clear();
            return None;
        }
        let last = len - 1;
        let target = match self.cursor {
            Some(current) => {
                let current = i64::try_from(current).unwrap_or(0);
                let moved = current.saturating_add(i64::from(delta));
                let clamped = moved.clamp(0, i64::try_from(last).unwrap_or(i64::MAX));
                usize::try_from(clamped).unwrap_or(0)
            }
            None if delta < 0 => last,
            None => 0,
        };
        self.extend_to(target);
        Some(target)
    }

    /// Drops any indices at or past `len`, and pulls the cursor back into
    /// range.
    ///
    /// Called after the backing list shrinks -- a refresh that picked up
    /// deletions, or a filter that narrowed the view. Without this, a stale
    /// index would paint a highlight on the wrong row, and acting on the
    /// selection would target whichever file happens to occupy that slot now.
    pub fn retain_below(&mut self, len: usize) {
        self.selected.retain(|&row| row < len);
        if len == 0 {
            self.cursor = None;
            self.anchor = None;
            return;
        }
        let last = len - 1;
        self.cursor = self.cursor.map(|c| c.min(last));
        self.anchor = self.anchor.map(|a| a.min(last));
    }
}

#[cfg(test)]
mod tests {
    use super::ListSelection;

    #[test]
    fn a_fresh_selection_is_empty_with_no_cursor() {
        let s = ListSelection::new();
        assert!(s.is_empty());
        assert_eq!(s.cursor(), None);
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn select_only_replaces_everything() {
        let mut s = ListSelection::new();
        s.select_only(3);
        s.select_only(7);
        assert_eq!(s.selected(), &[7]);
        assert_eq!(s.cursor(), Some(7));
    }

    #[test]
    fn toggle_adds_then_removes() {
        let mut s = ListSelection::new();
        s.toggle(2);
        assert!(s.contains(2));
        s.toggle(2);
        assert!(!s.contains(2));
    }

    #[test]
    fn toggled_rows_stay_sorted_regardless_of_click_order() {
        // Deterministic order is why this is a Vec and not a HashSet: "copy
        // the selection" should hand out paths in list order.
        let mut s = ListSelection::new();
        s.toggle(5);
        s.toggle(1);
        s.toggle(3);
        assert_eq!(s.selected(), &[1, 3, 5]);
    }

    #[test]
    fn extend_selects_the_whole_inclusive_span() {
        let mut s = ListSelection::new();
        s.select_only(2);
        s.extend_to(5);
        assert_eq!(s.selected(), &[2, 3, 4, 5]);
        assert_eq!(s.cursor(), Some(5));
    }

    #[test]
    fn extend_works_backwards() {
        let mut s = ListSelection::new();
        s.select_only(5);
        s.extend_to(2);
        assert_eq!(s.selected(), &[2, 3, 4, 5]);
        assert_eq!(s.cursor(), Some(2));
    }

    #[test]
    fn extending_twice_replaces_rather_than_accumulates() {
        // The anchor must survive the first extend, or the second
        // shift-click would grow the range instead of redefining it.
        let mut s = ListSelection::new();
        s.select_only(2);
        s.extend_to(6);
        s.extend_to(4);
        assert_eq!(s.selected(), &[2, 3, 4]);
    }

    #[test]
    fn extending_without_an_anchor_degrades_to_a_plain_select() {
        let mut s = ListSelection::new();
        s.extend_to(4);
        assert_eq!(s.selected(), &[4]);
        assert_eq!(s.cursor(), Some(4));
    }

    #[test]
    fn ctrl_click_reanchors_so_a_later_shift_click_starts_there() {
        let mut s = ListSelection::new();
        s.select_only(0);
        s.toggle(5);
        s.extend_to(7);
        assert_eq!(s.selected(), &[5, 6, 7], "range starts at the toggled row");
    }

    #[test]
    fn select_all_covers_the_list() {
        let mut s = ListSelection::new();
        s.select_all(4);
        assert_eq!(s.selected(), &[0, 1, 2, 3]);
        assert_eq!(s.cursor(), Some(0));
    }

    #[test]
    fn select_all_keeps_an_existing_cursor() {
        let mut s = ListSelection::new();
        s.select_only(2);
        s.select_all(5);
        assert_eq!(s.cursor(), Some(2));
    }

    #[test]
    fn select_all_on_an_empty_list_selects_nothing() {
        let mut s = ListSelection::new();
        s.select_all(0);
        assert!(s.is_empty());
    }

    #[test]
    fn the_first_down_press_lands_on_the_first_row() {
        let mut s = ListSelection::new();
        assert_eq!(s.move_cursor(1, 10), Some(0));
    }

    #[test]
    fn the_first_up_press_lands_on_the_last_row() {
        let mut s = ListSelection::new();
        assert_eq!(s.move_cursor(-1, 10), Some(9));
    }

    #[test]
    fn the_cursor_clamps_instead_of_wrapping() {
        let mut s = ListSelection::new();
        s.select_only(9);
        assert_eq!(s.move_cursor(1, 10), Some(9), "must not wrap to the top");
        s.select_only(0);
        assert_eq!(s.move_cursor(-1, 10), Some(0), "must not wrap to the bottom");
    }

    #[test]
    fn a_large_jump_clamps_to_the_ends() {
        let mut s = ListSelection::new();
        s.select_only(5);
        assert_eq!(s.move_cursor(i32::MAX, 10), Some(9));
        assert_eq!(s.move_cursor(i32::MIN, 10), Some(0));
    }

    #[test]
    fn moving_the_cursor_replaces_the_selection() {
        let mut s = ListSelection::new();
        s.select_only(1);
        s.extend_to(5);
        s.move_cursor(1, 10);
        assert_eq!(s.selected(), &[6], "a plain arrow key drops the range");
    }

    #[test]
    fn shift_arrow_grows_the_range_from_the_anchor() {
        let mut s = ListSelection::new();
        s.select_only(3);
        s.move_cursor_extending(1, 10);
        s.move_cursor_extending(1, 10);
        assert_eq!(s.selected(), &[3, 4, 5]);
        assert_eq!(s.cursor(), Some(5));
    }

    #[test]
    fn shift_arrow_back_over_the_anchor_shrinks_then_flips() {
        let mut s = ListSelection::new();
        s.select_only(3);
        s.move_cursor_extending(2, 10);
        assert_eq!(s.selected(), &[3, 4, 5]);
        s.move_cursor_extending(-3, 10);
        assert_eq!(s.selected(), &[2, 3], "range flips to the other side");
    }

    #[test]
    fn moving_in_an_empty_list_clears_everything() {
        let mut s = ListSelection::new();
        s.select_only(3);
        assert_eq!(s.move_cursor(1, 0), None);
        assert!(s.is_empty());
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn shrinking_drops_stale_indices() {
        // A stale index would highlight the wrong row and, worse, act on
        // whichever file now occupies that slot.
        let mut s = ListSelection::new();
        s.select_all(10);
        s.retain_below(4);
        assert_eq!(s.selected(), &[0, 1, 2, 3]);
    }

    #[test]
    fn shrinking_pulls_the_cursor_back_into_range() {
        let mut s = ListSelection::new();
        s.select_only(9);
        s.retain_below(5);
        assert_eq!(s.cursor(), Some(4));
    }

    #[test]
    fn shrinking_to_nothing_clears_the_cursor() {
        let mut s = ListSelection::new();
        s.select_only(3);
        s.retain_below(0);
        assert!(s.is_empty());
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn shrinking_leaves_an_in_range_selection_alone() {
        let mut s = ListSelection::new();
        s.select_only(2);
        s.retain_below(10);
        assert_eq!(s.selected(), &[2]);
        assert_eq!(s.cursor(), Some(2));
    }

    #[test]
    fn clearing_resets_the_cursor_too() {
        let mut s = ListSelection::new();
        s.select_only(3);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.cursor(), None);
    }

    #[test]
    fn selection_stays_sorted_and_unique_through_mixed_interactions() {
        // Invariant sweep: contains() is a binary search, so duplicates or
        // disorder would silently corrupt every later lookup.
        let mut s = ListSelection::new();
        s.select_only(4);
        s.toggle(9);
        s.toggle(1);
        s.extend_to(3);
        s.toggle(7);
        s.toggle(0);
        let rows = s.selected();
        assert!(rows.windows(2).all(|w| w[0] < w[1]), "sorted+unique: {rows:?}");
        for &row in rows {
            assert!(s.contains(row));
        }
    }
}
