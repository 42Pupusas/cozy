#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::missing_const_for_fn,
         clippy::must_use_candidate)]
//! 2-D cell buffer with scrollback.

use crate::Cell;

/// A rectangular grid of cells with optional scrollback.
pub struct Screen {
    /// All rows: `[scrollback rows ... | visible rows]`.
    /// Visible area starts at `buf[scrollback_len..]`.
    buf: Vec<Cell>,
    cols: u16,
    rows: u16,
    /// Maximum scrollback lines (0 for alternate screen).
    scrollback_max: usize,
    /// Current number of scrollback lines stored.
    scrollback_len: usize,
    /// Scroll region top (0-indexed, inclusive).
    scroll_top: u16,
    /// Scroll region bottom (0-indexed, inclusive).
    scroll_bot: u16,
}

impl Screen {
    pub fn new(cols: u16, rows: u16, scrollback_max: usize) -> Self {
        let total = usize::from(rows) + scrollback_max;
        Self {
            buf: vec![Cell::default(); total * usize::from(cols)],
            cols,
            rows,
            scrollback_max,
            scrollback_len: 0,
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
        }
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn scroll_region(&self) -> (u16, u16) {
        (self.scroll_top, self.scroll_bot)
    }

    pub fn set_scroll_region(&mut self, top: u16, bot: u16) {
        self.scroll_top = top;
        self.scroll_bot = bot.min(self.rows - 1);
    }

    /// Get the cell at `(col, row)` in the visible area.
    #[must_use]
    pub fn get_cell(&self, col: u16, row: u16) -> Cell {
        self.buf[self.visible_idx(col, row)]
    }

    /// Set the cell at `(col, row)` in the visible area.
    pub fn set_cell(&mut self, col: u16, row: u16, cell: Cell) {
        let idx = self.visible_idx(col, row);
        self.buf[idx] = cell;
    }

    /// Erase a rectangular region to blank cells.
    pub fn erase_range(&mut self, col0: u16, row0: u16, col1: u16, row1: u16) {
        for row in row0..=row1.min(self.rows - 1) {
            let c0 = if row == row0 { col0 } else { 0 };
            let c1 = if row == row1 { col1 } else { self.cols - 1 };
            self.erase_line_range(row, c0, c1);
        }
    }

    /// Erase columns `col0..=col1` on `row`.
    pub fn erase_line_range(&mut self, row: u16, col0: u16, col1: u16) {
        let base = self.visible_row_start(row);
        let c0 = usize::from(col0);
        let c1 = usize::from(col1).min(usize::from(self.cols) - 1);
        for cell in &mut self.buf[base + c0..=base + c1] {
            *cell = Cell::default();
        }
    }

    /// Clear entire visible area.
    pub fn clear(&mut self) {
        let start = self.scrollback_len * usize::from(self.cols);
        self.buf[start..].fill(Cell::default());
    }

    /// Scroll the region `[top, bot]` up by `n` lines, pulling blank lines at bottom.
    /// Lines that scroll off the top are pushed into scrollback (primary screen only).
    pub fn scroll_up(&mut self, n: u16, top: u16, bot: u16) {
        let cols = usize::from(self.cols);
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);

        if n == 0 || top_i >= bot_i {
            return;
        }

        // Push scrolled-off lines to scrollback if room.
        if top_i == 0 && self.scrollback_max > 0 {
            let push = n.min(self.scrollback_max - self.scrollback_len);
            if push > 0 {
                // Prepend scrollback: shift existing scrollback up and copy top visible rows.
                let sb = self.scrollback_len;
                let buf_cols = cols;
                // Rotate: move scrollback down by `push`, then write new lines at [sb..sb+push].
                // Because the buffer is [scrollback | visible], moving scrollback down means
                // moving it toward index 0 — scrollback grows toward higher indices in our layout.
                // New scrollback lines are appended after existing ones.
                let src = sb * buf_cols;
                let dst = 0;
                self.buf.copy_within(dst..src + push * buf_cols, 0);
                // Actually the simplest layout: scrollback at START, visible after.
                // scroll_up: copy n rows from visible top into scrollback end.
                let sb_start = self.scrollback_len * buf_cols;
                let vis_start = (self.scrollback_len + top_i) * buf_cols; // same as sb_start for top=0
                self.buf.copy_within(vis_start..vis_start + push * buf_cols, sb_start);
                self.scrollback_len += push;
            }
        }

        // Shift visible rows up within [top, bot].
        let vis_off = self.scrollback_len;
        let src_start = (vis_off + top_i + n) * cols;
        let dst_start = (vis_off + top_i) * cols;
        let move_rows = (bot_i - top_i + 1).saturating_sub(n);
        if move_rows > 0 {
            self.buf.copy_within(src_start..src_start + move_rows * cols, dst_start);
        }

        // Blank the bottom `n` rows.
        let blank_start = (vis_off + top_i + move_rows) * cols;
        self.buf[blank_start..blank_start + n.min(bot_i - top_i + 1) * cols].fill(Cell::default());
    }

    /// Scroll the region `[top, bot]` down by `n` lines, pulling blank lines at top.
    pub fn scroll_down(&mut self, n: u16, top: u16, bot: u16) {
        let cols = usize::from(self.cols);
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);
        let vis_off = self.scrollback_len;

        if n == 0 || top_i >= bot_i {
            return;
        }

        let move_rows = (bot_i - top_i + 1).saturating_sub(n);
        if move_rows > 0 {
            let src_start = (vis_off + top_i) * cols;
            let dst_start = (vis_off + top_i + n) * cols;
            self.buf.copy_within(src_start..src_start + move_rows * cols, dst_start);
        }
        // Blank top n rows.
        let blank_start = (vis_off + top_i) * cols;
        self.buf[blank_start..blank_start + n.min(bot_i - top_i + 1) * cols].fill(Cell::default());
    }

    /// Insert `n` blank lines at `row`, shifting rows in `[row, bot]` down.
    pub fn insert_lines(&mut self, row: u16, n: u16, bot: u16) {
        self.scroll_down(n, row, bot);
    }

    /// Delete `n` lines at `row`, pulling lines up from below.
    pub fn delete_lines(&mut self, row: u16, n: u16, bot: u16) {
        self.scroll_up(n, row, bot);
    }

    /// Delete `n` characters at `(col, row)`, shifting right chars left.
    pub fn delete_chars(&mut self, col: u16, row: u16, n: u16) {
        let cols = usize::from(self.cols);
        let base = self.visible_row_start(row);
        let col_i = usize::from(col);
        let n_i = usize::from(n).min(cols - col_i);
        let move_len = cols - col_i - n_i;
        if move_len > 0 {
            self.buf.copy_within(
                base + col_i + n_i..base + col_i + n_i + move_len,
                base + col_i,
            );
        }
        // Blank at right.
        self.buf[base + col_i + move_len..base + cols].fill(Cell::default());
    }

    /// Insert `n` blank characters at `(col, row)`, shifting right chars right.
    pub fn insert_chars(&mut self, col: u16, row: u16, n: u16) {
        let cols = usize::from(self.cols);
        let base = self.visible_row_start(row);
        let col_i = usize::from(col);
        let n_i = usize::from(n).min(cols - col_i);
        let move_len = cols - col_i - n_i;
        if move_len > 0 {
            self.buf.copy_within(
                base + col_i..base + col_i + move_len,
                base + col_i + n_i,
            );
        }
        self.buf[base + col_i..base + col_i + n_i].fill(Cell::default());
    }

    /// Resize, preserving as much content as possible.
    /// Resize to `new_cols` x `new_rows`, keeping the *bottom* of the content
    /// anchored (terminals fill upward from the last row, so the prompt and
    /// recent output must survive a shrink). `used_rows` is how many visible
    /// rows actually hold content (cursor row + 1); rows above the kept window
    /// spill into scrollback. Returns the number of rows shifted up into
    /// scrollback so the caller can adjust the cursor.
    pub fn resize(&mut self, new_cols: u16, new_rows: u16, used_rows: u16) -> u16 {
        let old_cols = usize::from(self.cols);
        let new_cols_u = usize::from(new_cols);
        let new_rows_u = usize::from(new_rows);
        let old_rows_u = usize::from(self.rows);
        let sb = self.scrollback_len;
        let copy_cols = old_cols.min(new_cols_u);

        // How many visible rows to carry over: the used region, clipped to the
        // new height. When shrinking below `used`, drop from the TOP (oldest).
        let used = usize::from(used_rows).clamp(1, old_rows_u);
        let keep = used.min(new_rows_u);
        let first_kept = used - keep; // rows above this are pushed to scrollback

        let total = new_rows_u + self.scrollback_max;
        let mut new_buf = vec![Cell::default(); total * new_cols_u];

        // Preserve existing scrollback (clipped to new width).
        for row in 0..sb.min(total.saturating_sub(new_rows_u)) {
            let src = row * old_cols;
            let dst = row * new_cols_u;
            new_buf[dst..dst + copy_cols].copy_from_slice(&self.buf[src..src + copy_cols]);
        }

        // Copy the kept visible rows, bottom-anchored to the top of the new
        // visible area.
        for i in 0..keep {
            let src = (sb + first_kept + i) * old_cols;
            let dst = (sb + i) * new_cols_u;
            new_buf[dst..dst + copy_cols].copy_from_slice(&self.buf[src..src + copy_cols]);
        }

        self.buf = new_buf;
        self.cols = new_cols;
        self.rows = new_rows;
        self.scroll_top = 0;
        self.scroll_bot = new_rows.saturating_sub(1);
        first_kept as u16
    }

    // ── private ───────────────────────────────────────────────────────────

    fn visible_row_start(&self, row: u16) -> usize {
        (self.scrollback_len + usize::from(row)) * usize::from(self.cols)
    }

    fn visible_idx(&self, col: u16, row: u16) -> usize {
        self.visible_row_start(row) + usize::from(col)
    }
}
