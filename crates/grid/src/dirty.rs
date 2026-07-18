#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_const_for_fn,
    clippy::manual_div_ceil
)]
//! Dirty-region tracker for incremental rendering.
//!
//! Tracks which rows have changed since the last [`DirtyRegion::take_snapshot`]
//! call. The renderer thread receives a [`DirtySnapshot`] and only redraws
//! the affected rows.

use std::collections::BTreeMap;

use crate::Cell;
use crate::screen::Screen;

/// Tracks dirty (modified) rows since the last snapshot.
pub struct DirtyRegion {
    /// Bitset of dirty rows. Each `u64` covers 64 rows.
    words: Vec<u64>,
    cols: u16,
    rows: u16,
}

impl DirtyRegion {
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        let words = (usize::from(rows) + 63) / 64;
        Self {
            words: vec![0u64; words],
            cols,
            rows,
        }
    }

    /// Mark a single cell dirty (marks its entire row).
    #[inline]
    pub(crate) fn mark(&mut self, _col: u16, row: u16) {
        let r = usize::from(row);
        self.words[r / 64] |= 1u64 << (r % 64);
    }

    /// Mark an entire row dirty.
    #[inline]
    pub(crate) fn mark_row(&mut self, row: u16) {
        let r = usize::from(row);
        if r < usize::from(self.rows) {
            self.words[r / 64] |= 1u64 << (r % 64);
        }
    }

    #[inline]
    fn get_bit(&self, r: usize) -> bool {
        (self.words[r / 64] >> (r % 64)) & 1 != 0
    }

    #[inline]
    fn set_bit(&mut self, r: usize, v: bool) {
        let w = &mut self.words[r / 64];
        let m = 1u64 << (r % 64);
        if v {
            *w |= m;
        } else {
            *w &= !m;
        }
    }

    /// Shift the dirty marks in `[top, bot]` up by `n` rows, to mirror a
    /// content scroll: a mark that was on row `r` follows its cells to row
    /// `r - n`. Marks that scroll off the top of the region are dropped; the
    /// `n` freshly-exposed rows at the bottom are marked dirty (they hold new
    /// blank content). Marks outside `[top, bot]` are left untouched.
    ///
    /// This is what lets a batch that mixes a scroll with in-place row edits
    /// produce a snapshot whose dirty rows are *exactly* the changed rows at
    /// their correct post-scroll positions -- instead of blanket-marking the
    /// whole region and forcing the renderer to guess which rows are genuine
    /// edits versus mere shifts.
    pub(crate) fn scroll_up(&mut self, n: u16, top: u16, bot: u16) {
        let n = usize::from(n);
        let top = usize::from(top);
        let bot = usize::from(bot).min(usize::from(self.rows).saturating_sub(1));
        if n == 0 || top > bot {
            return;
        }
        let n = n.min(bot - top + 1);
        // Ascending: we write row `r` (lower) after reading row `r + n`
        // (higher, not yet overwritten), so no mark is clobbered before use.
        for r in top..=bot {
            let src = r + n;
            let v = if src <= bot { self.get_bit(src) } else { true };
            self.set_bit(r, v);
        }
    }

    /// Mark a range of rows dirty (inclusive).
    pub(crate) fn mark_rows(&mut self, top: u16, bot: u16) {
        for row in top..=bot {
            self.mark_row(row);
        }
    }

    /// Mark every row dirty (full redraw).
    pub(crate) fn mark_all(&mut self) {
        for w in &mut self.words {
            *w = u64::MAX;
        }
    }

    /// Take a snapshot of current dirty rows and reset the tracker.
    ///
    /// The snapshot contains a copy of every dirty row's cells. `cursor`/
    /// `cursor_changed` are filled in by the caller ([`crate::Grid`]), which
    /// is the only place that knows the previous cursor state to diff
    /// against.
    pub(crate) fn take_snapshot(&mut self, screen: &Screen) -> DirtySnapshot {
        let mut rows = Vec::new();
        for (word_idx, &word) in self.words.iter().enumerate() {
            if word == 0 {
                continue;
            }
            let mut bits = word;
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                let row = (word_idx * 64 + bit) as u16;
                if row < self.rows {
                    let cells: Vec<Cell> = (0..self.cols)
                        .map(|col| screen.get_cell(col, row))
                        .collect();
                    rows.push(DirtyRow { row, cells });
                }
                bits &= bits - 1;
            }
        }
        // Reset.
        for w in &mut self.words {
            *w = 0;
        }
        DirtySnapshot {
            rows,
            cols: self.cols,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: SnapshotKind::Incremental { scroll_span: None },
        }
    }

    /// Reset the tracker without emitting any rows (a clean, empty snapshot).
    pub(crate) fn clear(&mut self) {
        for w in &mut self.words {
            *w = 0;
        }
    }
}

/// A row of cells that needs to be redrawn.
#[derive(Debug, Clone)]
pub struct DirtyRow {
    /// 0-indexed row number.
    pub row: u16,
    /// Cells in this row, left-to-right.
    pub cells: Vec<Cell>,
}

/// The cursor's position and visibility, carried on every [`DirtySnapshot`].
///
/// This lets the renderer track cursor movement even on frames where no cell
/// content changed (e.g. a bare cursor-movement escape with no glyph
/// printed) -- without this the renderer would have no way to know the
/// cursor moved and animate it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorInfo {
    /// Column (0-indexed).
    pub col: u16,
    /// Row (0-indexed).
    pub row: u16,
    /// Whether the cursor is visible (DECTCEM, `CSI ?25h`/`l`).
    pub visible: bool,
}

/// A band of rows `[top, bot]` whose content moved up by `delta` since the
/// last snapshot.
///
/// This is the unit of the terminal pixel-scroll optimization: the renderer
/// `copy_within`-shifts that pixel band up by `delta` rows, then paints the
/// snapshot's precise dirty rows on top.
///
/// Replaces the naked `(u16, u16, u16)` triple that used to thread through
/// `DirtySnapshot::scroll_span`, `PendingRepaint::Incremental`,
/// `Renderer::blit_snapshot`'s return, and `shift_overlay_rows` — four sites
/// that each had to remember which slot was which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollBand {
    /// First row of the band (inclusive).
    pub top: u16,
    /// Last row of the band (inclusive).
    pub bot: u16,
    /// Rows the band's content shifted up by.
    pub delta: u16,
}

impl ScrollBand {
    /// A real, paintable shift: a non-zero delta over a well-formed band.
    /// The `(0, 0, 0)` "mixed regions" sentinel and any zero-delta collapse
    /// to inactive — no pixel-shift, just repaint the dirty rows directly.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.delta > 0 && self.bot >= self.top
    }
}

/// Which kind of repaint a [`DirtySnapshot`] carries.
///
/// The two are mutually exclusive by construction -- a viewport repaint
/// redraws every visible row from scratch, which makes any incremental scroll
/// band moot -- and this enum makes that unrepresentable-when-wrong: you
/// cannot hold a viewport offset and a scroll band at once. Replaces a pair
/// of fields (`viewport_offset: Option<u16>` + `scroll_span: Option<ScrollBand>`)
/// that the renderer had to test *together* to recover the same choice, with
/// the illegal `(Some, Some)` state reachable but meaningless.
///
/// `content_delta` is deliberately *not* in here: it tracks text-vs-view
/// selection drift and is carried by both kinds, so it stays a plain field.
#[derive(Debug, Clone, Copy)]
pub enum SnapshotKind {
    /// Incremental repaint: `rows` lists the exact changed rows. `scroll_span`
    /// is `Some(band)` when everything in `[band.top, band.bot]` also shifted
    /// up by `band.delta`, letting the renderer `copy_within`-shift that pixel
    /// band and paint only the true dirty rows on top -- at their post-scroll
    /// positions, so an in-place edit batched alongside the scroll lands
    /// correctly. `None` when nothing scrolled or the fast path was abandoned
    /// (dirty rows are painted directly, no shift).
    Incremental { scroll_span: Option<ScrollBand> },
    /// Viewport repaint: the view window moved through scrollback to `offset`
    /// lines up from the live bottom (`0` = bottom).
    ///
    /// `shift` is the pixel-scroll optimization for the common case of a small
    /// wheel/page step: the whole screen shifted by `shift` rows (signed --
    /// negative = view moved up into history, revealing rows at the *top*;
    /// positive = view moved back toward the bottom, revealing rows at the
    /// *bottom*), and `rows` carries *only* the freshly-revealed edge rows. The
    /// renderer `copy_within`-shifts the previous viewport frame and repaints
    /// just those, instead of re-rasterizing every glyph on screen -- the whole
    /// reason scrolling back stayed at full-repaint cost before.
    ///
    /// `shift: None` is a full-frame repaint: `rows` covers every visible row.
    /// Used for the first scroll, a jump too large to shift (`|shift| >= rows`,
    /// nothing on screen survives), or a snap back to the bottom on new output.
    Viewport { offset: u16, shift: Option<i32> },
}

/// Snapshot of all rows that changed since the last render.
///
/// Sent from the grid-thread to the renderer-thread via SPSC ring.
#[derive(Debug, Clone)]
pub struct DirtySnapshot {
    /// Changed rows (sorted ascending by row index).
    pub rows: Vec<DirtyRow>,
    /// Column count (grid width).
    pub cols: u16,
    /// The cursor's current position/visibility.
    pub cursor: CursorInfo,
    /// True if `cursor` differs from the last snapshot sent -- lets the
    /// renderer detect "the cursor moved but no cell changed" so it still
    /// repaints (and animates) even when `rows` is empty.
    pub cursor_changed: bool,
    /// Net lines the primary screen's content scrolled up since the last
    /// snapshot (new output pushing lines into scrollback). The renderer
    /// shifts its selection and cell mirror up by this so a highlight stays
    /// glued to the *text*, not the viewport position, as output streams in.
    /// `0` on the alternate screen. Carried by both snapshot kinds.
    pub content_delta: u16,
    /// Incremental (dirty-row) vs. full-frame viewport repaint -- and, for the
    /// incremental case, the optional pixel-scroll band. See [`SnapshotKind`].
    pub kind: SnapshotKind,
}

impl DirtySnapshot {
    /// The pixel-scroll band, if this is an incremental snapshot carrying one.
    /// `None` for viewport snapshots and for incremental ones that didn't
    /// scroll. (Compose relies on this being incremental-only; a viewport
    /// shift is exposed separately via [`Self::pixel_shift`].)
    #[must_use]
    pub fn scroll_span(&self) -> Option<ScrollBand> {
        match self.kind {
            SnapshotKind::Incremental { scroll_span } => scroll_span,
            SnapshotKind::Viewport { .. } => None,
        }
    }

    /// The signed whole-screen shift for a viewport (scrollback) snapshot, if
    /// it carries the pixel-scroll optimization. Negative = view moved up into
    /// history (reveal at top); positive = view moved toward the bottom (reveal
    /// at bottom). `None` for full-frame viewport repaints and for every
    /// incremental snapshot (which use [`Self::scroll_span`] instead).
    #[must_use]
    pub fn viewport_shift(&self) -> Option<i32> {
        match self.kind {
            SnapshotKind::Viewport { shift, .. } => shift,
            SnapshotKind::Incremental { .. } => None,
        }
    }

    /// The scrollback offset when this is a viewport snapshot; `None` for
    /// incremental (live-bottom) snapshots.
    #[must_use]
    pub fn viewport_offset(&self) -> Option<u16> {
        match self.kind {
            SnapshotKind::Viewport { offset, .. } => Some(offset),
            SnapshotKind::Incremental { .. } => None,
        }
    }

    /// True if no rows changed AND the cursor didn't move/toggle visibility
    /// either -- i.e. there is genuinely nothing new for the renderer to do.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && !self.cursor_changed && self.content_delta == 0
    }

    /// Fold `next` -- a snapshot produced *after* `self` -- into `self`, so the
    /// single result, blitted once, reproduces the effect of blitting `self`
    /// then `next` in order. Returns `true` on success; `false` when the two
    /// cannot be composed (a full-frame viewport repaint, a geometry change,
    /// or two *different* scroll bands in the run), in which case the caller
    /// must flush `self` and start a fresh accumulator at `next`.
    ///
    /// This is the render-side dual of [`DirtyRegion::scroll_up`]: a run of
    /// scroll snapshots over the same band `[top, bot]` composes to one shift
    /// of the summed delta, with each already-accumulated row relabelled up by
    /// the *newer* snapshot's delta (rows pushed above `top` scroll off and are
    /// dropped), then the newer rows painted on top last-writer-wins. Under a
    /// scroll flood this collapses a ~30-deep backlog of full-band memmoves
    /// into a single blit -- which is the whole point.
    #[must_use]
    pub fn compose(&mut self, next: &Self) -> bool {
        // Full-frame viewport snapshots repaint everything from scratch; a
        // geometry change means the row indices aren't comparable. Neither
        // composes -- bail so the caller flushes.
        if self.viewport_offset().is_some() || next.viewport_offset().is_some() {
            return false;
        }
        if self.cols != next.cols {
            return false;
        }
        // The composed scroll band. Same-band shifts add; a scroll on only one
        // side carries through; two *different* bands can't be expressed as one
        // shift, so refuse.
        let band = match (self.scroll_span(), next.scroll_span()) {
            (existing, None) => existing,
            (None, Some(b)) => Some(b),
            (Some(a), Some(b)) => {
                if a.top == b.top && a.bot == b.bot {
                    Some(ScrollBand {
                        top: a.top,
                        bot: a.bot,
                        delta: a.delta.saturating_add(b.delta),
                    })
                } else {
                    return false;
                }
            }
        };
        // Accumulate self's rows, relabelled up by `next`'s scroll delta (its
        // shift runs *after* self's paint, so it drags self's painted rows up
        // too). Rows shifted above the band top scroll off and vanish; rows
        // outside the band are untouched. A BTreeMap keeps the result sorted
        // ascending, as the `rows` invariant requires.
        let mut map: BTreeMap<u16, DirtyRow> = BTreeMap::new();
        if let Some(ScrollBand { top, bot, delta }) = next.scroll_span() {
            for row in self.rows.drain(..) {
                let r = row.row;
                if r >= top && r <= bot {
                    if let Some(nr) = r.checked_sub(delta)
                        && nr >= top
                    {
                        map.insert(
                            nr,
                            DirtyRow {
                                row: nr,
                                cells: row.cells,
                            },
                        );
                    }
                    // else: scrolled off the top -- drop it.
                } else {
                    map.insert(r, row);
                }
            }
        } else {
            for row in self.rows.drain(..) {
                map.insert(row.row, row);
            }
        }
        // Paint `next`'s rows on top -- newest wins on any row they both touch.
        for row in &next.rows {
            map.insert(row.row, row.clone());
        }
        self.rows = map.into_values().collect();
        // Both sides are Incremental here (viewport bailed above), so this
        // preserves the kind and just updates the composed band.
        self.kind = SnapshotKind::Incremental { scroll_span: band };
        self.cursor = next.cursor;
        self.cursor_changed = self.cursor_changed || next.cursor_changed;
        self.content_delta = self.content_delta.saturating_add(next.content_delta);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirty_rows(d: &DirtyRegion) -> Vec<u16> {
        (0..d.rows).filter(|&r| d.get_bit(usize::from(r))).collect()
    }

    #[test]
    fn scroll_up_shifts_marks_and_dirties_exposed_bottom() {
        // 8-row region. Mark rows 2 and 5 dirty (in-place edits), then scroll
        // the whole screen up by 2. The marks must follow their content to
        // rows 0 and 3, and the two freshly-exposed bottom rows (6, 7) must be
        // newly dirty -- nothing else.
        let mut d = DirtyRegion::new(4, 8);
        d.mark_row(2);
        d.mark_row(5);
        d.scroll_up(2, 0, 7);
        assert_eq!(dirty_rows(&d), vec![0, 3, 6, 7]);
    }

    #[test]
    fn scroll_up_drops_marks_scrolled_off_the_top() {
        // A mark within the scrolled-off band (row 1, shifted by 3) lands at
        // row -2 -- gone. Row 4 -> row 1 survives.
        let mut d = DirtyRegion::new(4, 8);
        d.mark_row(1);
        d.mark_row(4);
        d.scroll_up(3, 0, 7);
        assert_eq!(dirty_rows(&d), vec![1, 5, 6, 7]);
    }

    #[test]
    fn scroll_up_within_a_subregion_leaves_outside_marks_untouched() {
        // Scroll region [2, 5]. A mark above (row 0) and below (row 7) the
        // region must not move; inside, row 4 -> row 3 and the exposed bottom
        // (row 5) is dirtied.
        let mut d = DirtyRegion::new(4, 8);
        d.mark_row(0);
        d.mark_row(4);
        d.mark_row(7);
        d.scroll_up(1, 2, 5);
        assert_eq!(dirty_rows(&d), vec![0, 3, 5, 7]);
    }

    #[test]
    fn scroll_up_by_zero_or_reversed_region_is_noop() {
        let mut d = DirtyRegion::new(4, 8);
        d.mark_row(3);
        d.scroll_up(0, 0, 7);
        assert_eq!(dirty_rows(&d), vec![3]);
        // Reversed region (top > bot): must not panic and must not change marks.
        d.scroll_up(2, 5, 2);
        assert_eq!(dirty_rows(&d), vec![3]);
    }

    fn cell(ch: char) -> Cell {
        Cell {
            ch,
            ..Cell::default()
        }
    }

    fn band(top: u16, bot: u16, delta: u16) -> ScrollBand {
        ScrollBand { top, bot, delta }
    }

    fn snap(cols: u16, scroll: Option<(u16, u16, u16)>, rows: &[(u16, char)]) -> DirtySnapshot {
        DirtySnapshot {
            rows: rows
                .iter()
                .map(|&(r, ch)| DirtyRow {
                    row: r,
                    cells: vec![cell(ch)],
                })
                .collect(),
            cols,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: SnapshotKind::Incremental {
                scroll_span: scroll.map(|(t, b, d)| band(t, b, d)),
            },
        }
    }

    fn row_map(s: &DirtySnapshot) -> Vec<(u16, char)> {
        s.rows.iter().map(|r| (r.row, r.cells[0].ch)).collect()
    }

    #[test]
    fn compose_sums_deltas_of_the_same_scroll_band() {
        // Two whole-screen scrolls of an 8-row grid: up by 2, then up by 3.
        // The composed band is a single up-shift of 5.
        let mut a = snap(4, Some((0, 7, 2)), &[(6, 'a'), (7, 'b')]);
        let b = snap(4, Some((0, 7, 3)), &[(5, 'c'), (6, 'd'), (7, 'e')]);
        assert!(a.compose(&b));
        assert_eq!(a.scroll_span(), Some(band(0, 7, 5)));
        // a's freshly-painted rows 6,7 drag up by b's delta 3 -> rows 3,4;
        // b's rows 5,6,7 sit on top. All sorted ascending.
        assert_eq!(
            row_map(&a),
            vec![(3, 'a'), (4, 'b'), (5, 'c'), (6, 'd'), (7, 'e')]
        );
    }

    #[test]
    fn compose_drops_rows_scrolled_off_the_top() {
        // a paints row 1; b scrolls up by 3, dragging row 1 to row -2 -> gone.
        let mut a = snap(4, Some((0, 7, 1)), &[(1, 'x'), (7, 'a')]);
        let b = snap(4, Some((0, 7, 3)), &[(7, 'b')]);
        assert!(a.compose(&b));
        // row 1 -> -2 dropped; row 7 -> 4 kept; b's row 7 on top.
        assert_eq!(row_map(&a), vec![(4, 'a'), (7, 'b')]);
    }

    #[test]
    fn compose_last_writer_wins_on_shared_rows() {
        // No scroll: same row touched by both -> newest content wins.
        let mut a = snap(4, None, &[(2, 'o'), (3, 'o')]);
        let b = snap(4, None, &[(3, 'n'), (4, 'n')]);
        assert!(a.compose(&b));
        assert_eq!(row_map(&a), vec![(2, 'o'), (3, 'n'), (4, 'n')]);
        assert_eq!(a.scroll_span(), None);
    }

    #[test]
    fn compose_refuses_conflicting_bands() {
        let mut a = snap(4, Some((0, 7, 1)), &[(7, 'a')]);
        let b = snap(4, Some((2, 5, 1)), &[(5, 'b')]);
        assert!(!a.compose(&b));
    }

    #[test]
    fn compose_refuses_viewport_and_geometry_changes() {
        let a = snap(4, None, &[(0, 'a')]);
        let mut vp = snap(4, None, &[(0, 'b')]);
        vp.kind = SnapshotKind::Viewport {
            offset: 3,
            shift: None,
        };
        assert!(!a.clone().compose(&vp));
        assert!(!vp.compose(&a));
        // geometry change (different cols)
        let mut a2 = snap(4, None, &[(0, 'a')]);
        let b2 = snap(6, None, &[(0, 'b')]);
        assert!(!a2.compose(&b2));
    }

    #[test]
    fn compose_carries_a_one_sided_scroll_through() {
        // a has no scroll, b scrolls: a's rows drag up by b's delta.
        let mut a = snap(4, None, &[(5, 'a')]);
        let b = snap(4, Some((0, 7, 2)), &[(7, 'b')]);
        assert!(a.compose(&b));
        assert_eq!(a.scroll_span(), Some(band(0, 7, 2)));
        assert_eq!(row_map(&a), vec![(3, 'a'), (7, 'b')]);
    }
}
