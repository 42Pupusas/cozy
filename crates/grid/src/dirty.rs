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
    /// The snapshot contains a copy of every dirty row's cells.
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
        }
    }

    /// Reset the tracker without emitting any rows (a clean, empty snapshot).
    pub(crate) fn clear(&mut self) {
        for w in &mut self.words {
            *w = 0;
        }
    }
}

/// Build a full-frame snapshot of the whole visible screen through a viewport
/// offset (`0` = live view at the bottom; `k` = scrolled `k` lines into
/// scrollback). Used when the viewport moves: every row's content changes, so
/// the renderer needs a complete repaint rather than incremental dirty rows.
pub(crate) fn viewport_snapshot(
    screen: &Screen,
    viewport_offset: usize,
    cols: u16,
    rows: u16,
) -> DirtySnapshot {
    let mut out = Vec::with_capacity(usize::from(rows));
    for row in 0..rows {
        let cells: Vec<Cell> = (0..cols)
            .map(|col| screen.get_viewport_cell(col, row, viewport_offset))
            .collect();
        out.push(DirtyRow { row, cells });
    }
    DirtySnapshot { rows: out, cols }
}

/// A row of cells that needs to be redrawn.
#[derive(Debug, Clone)]
pub struct DirtyRow {
    /// 0-indexed row number.
    pub row: u16,
    /// Cells in this row, left-to-right.
    pub cells: Vec<Cell>,
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
}

impl DirtySnapshot {
    /// True if no rows changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}
