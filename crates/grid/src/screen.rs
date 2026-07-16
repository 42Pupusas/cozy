#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate
)]
//! 2-D cell buffer with scrollback.

use crate::Cell;

/// A rectangular grid of cells with optional scrollback.
pub struct Screen {
    /// All rows: `[scrollback rows ... | visible rows]`.
    /// Visible area starts at `buf[scrollback_len..]`.
    buf: Vec<Cell>,
    /// Per physical row: true if the line continues (soft-wrapped) onto the
    /// next physical row. Parallel to rows in `buf`; length `total`.
    /// This is what lets `resize` reflow instead of clip.
    wrapped: Vec<bool>,
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
            wrapped: vec![false; total],
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

    /// Mark (or clear) the soft-wrap continuation flag for a visible row.
    pub fn set_wrapped(&mut self, row: u16, wrapped: bool) {
        let idx = self.scrollback_len + usize::from(row);
        if idx < self.wrapped.len() {
            self.wrapped[idx] = wrapped;
        }
    }

    /// Whether a visible row soft-wraps onto the next.
    #[must_use]
    pub fn is_wrapped(&self, row: u16) -> bool {
        let idx = self.scrollback_len + usize::from(row);
        self.wrapped.get(idx).copied().unwrap_or(false)
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
        let mut c0 = usize::from(col0);
        let mut c1 = usize::from(col1).min(usize::from(self.cols) - 1);
        // Never orphan half of a wide-char pair: if the erase boundary falls
        // on a spacer cell, pull c0 left onto its lead; if it falls on a wide
        // lead cell, push c1 right onto its spacer.
        if c0 < self.cols as usize && self.buf[base + c0].spacer && c0 > 0 {
            c0 -= 1;
        }
        if c1 < self.cols as usize && self.buf[base + c1].wide && c1 + 1 < self.cols as usize {
            c1 += 1;
        }
        for cell in &mut self.buf[base + c0..=base + c1] {
            *cell = Cell::default();
        }
        // Erasing through the right margin breaks any soft-wrap continuation.
        if c1 + 1 >= usize::from(self.cols) {
            self.set_wrapped(row, false);
        }
    }

    /// Clear entire visible area.
    pub fn clear(&mut self) {
        let start = self.scrollback_len * usize::from(self.cols);
        self.buf[start..].fill(Cell::default());
        for w in &mut self.wrapped[self.scrollback_len..] {
            *w = false;
        }
    }

    /// Scroll the region `[top, bot]` up by `n` lines, pulling blank lines at
    /// bottom, for a genuine "content advanced past the bottom margin" event
    /// (linefeed past the last row, or an explicit `CSI Ps S` Scroll Up).
    /// Lines that scroll off the top are pushed into scrollback (primary
    /// screen only, and only when the region starts at row 0).
    pub fn scroll_up(&mut self, n: u16, top: u16, bot: u16) {
        self.scroll_up_impl(n, top, bot, true);
    }

    /// Delete `n` lines at `row` (DL, `CSI Ps M`): pulls lines below up into
    /// the gap. Unlike a genuine scroll, DL is a line-editing operation, not
    /// output advancing past a margin -- the deleted lines must simply
    /// disappear, never get appended to scrollback (xterm/VTE/kitty all
    /// agree DL/IL never touch history).
    pub fn delete_lines_no_scrollback(&mut self, row: u16, n: u16, bot: u16) {
        self.scroll_up_impl(n, row, bot, false);
    }

    fn scroll_up_impl(&mut self, n: u16, top: u16, bot: u16, push_scrollback: bool) {
        let cols = usize::from(self.cols);
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);

        if n == 0 || top_i >= bot_i {
            return;
        }

        // Push scrolled-off lines to scrollback if room (top region only).
        if push_scrollback && top_i == 0 && self.scrollback_max > 0 {
            let push = n.min(self.scrollback_max - self.scrollback_len);
            if push > 0 {
                // The top `push` visible rows simply become scrollback: they
                // are already contiguous just below existing scrollback, so we
                // only need to extend the boundary (their wrapped flags ride
                // along untouched).
                self.scrollback_len += push;
            }
        }

        // Shift visible rows up within [top, bot].
        let vis_off = self.scrollback_len;
        let src_start = (vis_off + top_i + n) * cols;
        let dst_start = (vis_off + top_i) * cols;
        let move_rows = (bot_i - top_i + 1).saturating_sub(n);
        if move_rows > 0 {
            self.buf
                .copy_within(src_start..src_start + move_rows * cols, dst_start);
            self.wrapped.copy_within(
                vis_off + top_i + n..vis_off + top_i + n + move_rows,
                vis_off + top_i,
            );
        }

        // Blank the bottom `n` rows.
        let blank_n = n.min(bot_i - top_i + 1);
        let blank_start = (vis_off + top_i + move_rows) * cols;
        self.buf[blank_start..blank_start + blank_n * cols].fill(Cell::default());
        for w in
            &mut self.wrapped[vis_off + top_i + move_rows..vis_off + top_i + move_rows + blank_n]
        {
            *w = false;
        }
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
            self.buf
                .copy_within(src_start..src_start + move_rows * cols, dst_start);
            self.wrapped.copy_within(
                vis_off + top_i..vis_off + top_i + move_rows,
                vis_off + top_i + n,
            );
        }
        // Blank top n rows.
        let blank_n = n.min(bot_i - top_i + 1);
        let blank_start = (vis_off + top_i) * cols;
        self.buf[blank_start..blank_start + blank_n * cols].fill(Cell::default());
        for w in &mut self.wrapped[vis_off + top_i..vis_off + top_i + blank_n] {
            *w = false;
        }
    }

    /// Insert `n` blank lines at `row`, shifting rows in `[row, bot]` down.
    pub fn insert_lines(&mut self, row: u16, n: u16, bot: u16) {
        self.scroll_down(n, row, bot);
    }

    /// Delete `n` lines at `row`, pulling lines up from below. A
    /// line-editing operation (DL), not a scroll -- must never push into
    /// scrollback, unlike `scroll_up`.
    pub fn delete_lines(&mut self, row: u16, n: u16, bot: u16) {
        self.delete_lines_no_scrollback(row, n, bot);
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
        // The shift can slice a wide-char pair in half at either seam.
        self.sanitize_wide_pairs(row);
    }

    /// Insert `n` blank characters at `(col, row)`, shifting right chars right.
    pub fn insert_chars(&mut self, col: u16, row: u16, n: u16) {
        let cols = usize::from(self.cols);
        let base = self.visible_row_start(row);
        let col_i = usize::from(col);
        let n_i = usize::from(n).min(cols - col_i);
        let move_len = cols - col_i - n_i;
        if move_len > 0 {
            self.buf
                .copy_within(base + col_i..base + col_i + move_len, base + col_i + n_i);
        }
        self.buf[base + col_i..base + col_i + n_i].fill(Cell::default());
        self.sanitize_wide_pairs(row);
    }

    /// Scan a row and blank any wide-char cell whose spacer partner is
    /// missing/wrong (or vice versa). Called after any operation that can
    /// shift cells without moving their pair atomically (delete/insert
    /// chars), preventing an orphaned half-glyph from lingering on screen.
    fn sanitize_wide_pairs(&mut self, row: u16) {
        let cols = usize::from(self.cols);
        let base = self.visible_row_start(row);
        let mut col = 0usize;
        while col < cols {
            let cell = self.buf[base + col];
            if cell.wide {
                let has_partner = col + 1 < cols && self.buf[base + col + 1].spacer;
                if has_partner {
                    col += 1; // skip the valid spacer partner
                } else {
                    self.buf[base + col] = Cell::default();
                }
            } else if cell.spacer {
                // A spacer with no wide lead directly to its left is orphaned.
                let has_lead = col > 0 && self.buf[base + col - 1].wide;
                if !has_lead {
                    self.buf[base + col] = Cell::default();
                }
            }
            col += 1;
        }
    }

    /// Resize to `new_cols` x `new_rows`, **reflowing** soft-wrapped lines to
    /// the new width instead of clipping them.
    ///
    /// The whole buffer (scrollback + visible) is split into logical lines by
    /// following the per-row `wrapped` flags, each logical line is re-broken at
    /// `new_cols`, and the result is repacked bottom-anchored (overflow spills
    /// into scrollback). `cursor` is the active cursor in the OLD geometry; the
    /// returned pair is its position in the NEW geometry.
    #[allow(clippy::too_many_lines)]
    pub fn resize(&mut self, new_cols: u16, new_rows: u16, cursor: (u16, u16)) -> (u16, u16) {
        let old_cols = usize::from(self.cols);
        let new_cols_u = usize::from(new_cols).max(1);
        let new_rows_u = usize::from(new_rows).max(1);
        let total_old = self.scrollback_len + usize::from(self.rows);
        let cursor_abs = self.scrollback_len + usize::from(cursor.1);
        let cursor_col = usize::from(cursor.0);

        // 1. Stitch physical rows into logical lines via the wrapped flags,
        //    recording where the cursor falls.
        let mut lines: Vec<Vec<Cell>> = Vec::new();
        let mut cursor_line = 0usize;
        let mut cursor_offset = 0usize;
        let mut r = 0usize;
        while r < total_old {
            let li = lines.len();
            let mut cells: Vec<Cell> = Vec::new();
            loop {
                let base = r * old_cols;
                if r == cursor_abs {
                    cursor_line = li;
                    cursor_offset = cells.len() + cursor_col;
                }
                cells.extend_from_slice(&self.buf[base..base + old_cols]);
                let cont = self.wrapped.get(r).copied().unwrap_or(false);
                r += 1;
                if !cont || r >= total_old {
                    break;
                }
            }
            // Trim trailing blanks (the unused tail of the logical line).
            while cells.last() == Some(&Cell::default()) {
                cells.pop();
            }
            lines.push(cells);
        }

        // 2. Drop trailing empty logical lines, but never above the cursor line.
        let mut last_keep = cursor_line;
        for (i, l) in lines.iter().enumerate() {
            if !l.is_empty() {
                last_keep = last_keep.max(i);
            }
        }
        lines.truncate(last_keep + 1);

        // 3. Re-break each logical line at the new width.
        let mut rows_out: Vec<(Vec<Cell>, bool)> = Vec::new();
        let mut new_cursor_idx = 0usize;
        let mut new_cursor_col = 0usize;
        for (li, line) in lines.iter().enumerate() {
            let pieces = if line.is_empty() {
                1
            } else {
                line.len().div_ceil(new_cols_u)
            };
            let base_idx = rows_out.len();
            for p in 0..pieces {
                let start = p * new_cols_u;
                let end = ((p + 1) * new_cols_u).min(line.len());
                let mut row_cells = vec![Cell::default(); new_cols_u];
                if start < end {
                    row_cells[..end - start].copy_from_slice(&line[start..end]);
                }
                // The naive slice-at-new_cols_u break can land exactly on a
                // wide-char pair's seam, leaving an orphaned lead (no spacer
                // to its right) or an orphaned spacer (no lead to its left).
                // Blank the orphan rather than let a half-glyph render or
                // corrupt the next row's read. (Full width-aware line
                // breaking that never splits a pair is a follow-up.)
                if let Some(last) = row_cells.last_mut()
                    && last.wide
                {
                    *last = Cell::default();
                }
                if let Some(first) = row_cells.first_mut()
                    && first.spacer
                {
                    *first = Cell::default();
                }
                rows_out.push((row_cells, p + 1 < pieces));
            }
            if li == cursor_line {
                let piece = cursor_offset / new_cols_u;
                if piece >= pieces {
                    // The cursor sits one position past the line's content (an
                    // eager-wrap terminal parks it at the start of a fresh row
                    // after filling the last column). That row was trimmed as a
                    // trailing blank, so re-materialize it instead of clamping
                    // the cursor back onto the final glyph — clamping is what
                    // made the shell's SIGWINCH repaint clear the wrong rows.
                    rows_out.push((vec![Cell::default(); new_cols_u], false));
                    new_cursor_idx = rows_out.len() - 1;
                    new_cursor_col = cursor_offset - piece * new_cols_u;
                } else {
                    new_cursor_idx = base_idx + piece;
                    new_cursor_col = (cursor_offset - piece * new_cols_u).min(new_cols_u - 1);
                }
            }
        }

        // 4. Repack bottom-anchored: the last `new_rows` go visible, the rest
        //    into scrollback (capped, dropping the oldest beyond the cap).
        let m = rows_out.len();
        let overflow = m.saturating_sub(new_rows_u);
        let new_sb_len = overflow.min(self.scrollback_max);
        let dropped = overflow - new_sb_len;
        let visible_count = m - overflow;

        let total = new_rows_u + self.scrollback_max;
        let mut new_buf = vec![Cell::default(); total * new_cols_u];
        let mut new_wrapped = vec![false; total];

        for i in 0..new_sb_len {
            let (cells, w) = &rows_out[dropped + i];
            let dst = i * new_cols_u;
            new_buf[dst..dst + new_cols_u].copy_from_slice(cells);
            new_wrapped[i] = *w;
        }
        for i in 0..visible_count {
            let (cells, w) = &rows_out[overflow + i];
            let dst = (new_sb_len + i) * new_cols_u;
            new_buf[dst..dst + new_cols_u].copy_from_slice(cells);
            new_wrapped[new_sb_len + i] = *w;
        }

        self.buf = new_buf;
        self.wrapped = new_wrapped;
        // Store the CLAMPED dimensions (>= 1): the buffer was allocated with
        // new_cols_u/new_rows_u, and a 0 here would desync the stride and
        // overflow every `cols - 1` / `rows - 1` elsewhere.
        self.cols = new_cols_u as u16;
        self.rows = new_rows_u as u16;
        self.scrollback_len = new_sb_len;
        self.scroll_top = 0;
        self.scroll_bot = self.rows.saturating_sub(1);

        // 5. Map the cursor into the new geometry.
        let new_row = if new_cursor_idx >= overflow {
            (new_cursor_idx - overflow) as u16
        } else {
            0
        };
        (
            new_cursor_col as u16,
            new_row.min(new_rows.saturating_sub(1)),
        )
    }

    /// Index of the last visible row that holds any non-blank cell, plus one
    /// (i.e. the number of visible rows actually occupied by content). Returns
    /// 0 when the visible area is entirely blank.
    #[must_use]
    pub fn content_rows(&self) -> u16 {
        let cols = usize::from(self.cols);
        for row in (0..usize::from(self.rows)).rev() {
            let start = (self.scrollback_len + row) * cols;
            if self.buf[start..start + cols]
                .iter()
                .any(|c| *c != Cell::default())
            {
                return row as u16 + 1;
            }
        }
        0
    }

    // ── private ───────────────────────────────────────────────────────────

    fn visible_row_start(&self, row: u16) -> usize {
        (self.scrollback_len + usize::from(row)) * usize::from(self.cols)
    }

    fn visible_idx(&self, col: u16, row: u16) -> usize {
        self.visible_row_start(row) + usize::from(col)
    }
}
