#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate
)]
//! 2-D cell buffer with scrollback.

use crate::Cell;

/// A rectangular grid of cells with optional scrollback.
///
/// The logical `[scrollback | visible]` window does NOT have to sit at the
/// front of `buf`. It floats at physical row offset `head`, and `buf` is
/// allocated with `scrollback_max` rows of *slack* past the maximum window
/// size. This is what makes a sustained flood cheap: once scrollback is full,
/// evicting the oldest line is just `head += 1` (the surviving rows never
/// move), instead of an O(scrollback) slide-to-front on every single line --
/// which turned a long flood quadratic and stalled it outright. When `head`
/// walks off the end of the slack, we compact once (move the window back to
/// row 0), giving amortized O(1) eviction.
pub struct Screen {
    /// Backing store, `cap_rows(rows, scrollback_max)` rows tall. The live
    /// window occupies physical rows `[head .. head + scrollback_len + rows)`.
    buf: Vec<Cell>,
    /// Per physical row: true if the line continues (soft-wrapped) onto the
    /// next physical row. Parallel to rows in `buf`; same length as `buf/cols`.
    /// This is what lets `resize` reflow instead of clip.
    wrapped: Vec<bool>,
    cols: u16,
    rows: u16,
    /// Maximum scrollback lines (0 for alternate screen).
    scrollback_max: usize,
    /// Current number of scrollback lines stored.
    scrollback_len: usize,
    /// Physical row where the window (absolute row 0 = oldest scrollback row)
    /// currently begins. Advanced cheaply on eviction; reset to 0 on compaction
    /// and whenever the buffer is rebuilt (`resize`, `erase_scrollback`).
    head: usize,
    /// Scroll region top (0-indexed, inclusive).
    scroll_top: u16,
    /// Scroll region bottom (0-indexed, inclusive).
    scroll_bot: u16,
    /// Fast-path guard: `true` guarantees the whole visible area is blank
    /// (`Cell::default()`) with no soft-wrap flags set, so a `clear()` can be
    /// skipped entirely. Set by `clear()`; cleared by the only two methods that
    /// can inject non-blank state (`set_cell`, `set_wrapped`) -- `print()`
    /// routes through both. Every other mutator (erase/scroll) only ever
    /// produces blanks from blanks, so it preserves the flag untouched. This is
    /// what makes the alt-screen-thrash workload (smcup/rmcup + a redundant ED2
    /// on every flip) cheap: the ED2 clear lands on an already-blank buffer and
    /// becomes a no-op instead of a full-screen memset.
    all_blank: bool,
}

impl Screen {
    /// Total rows to allocate: twice the biggest possible window
    /// (`rows + scrollback_max`). The window is at most `rows + scrollback_max`
    /// rows; the other half is slack the floating `head` advances into before a
    /// compaction is needed. With `slack == rows + scrollback_max` and each
    /// eviction advancing `head` by `n` rows, a compaction (which moves the whole
    /// window, ~`rows + scrollback_max` rows) is amortized over ~`(rows +
    /// scrollback_max) / n` evictions -> O(n) row-copies per scroll, never the
    /// O(scrollback) slide-to-front that stalled sustained floods.
    ///
    /// Was a free `cap_rows(rows, scrollback_max)` function -- moved onto
    /// `Screen` as an associated function since it computes a `Screen`
    /// sizing invariant, not a standalone calculation.
    fn cap_rows(rows: usize, scrollback_max: usize) -> usize {
        2 * (rows + scrollback_max)
    }

    pub fn new(cols: u16, rows: u16, scrollback_max: usize) -> Self {
        let total = Self::cap_rows(usize::from(rows), scrollback_max);
        Self {
            buf: vec![Cell::default(); total * usize::from(cols)],
            wrapped: vec![false; total],
            cols,
            rows,
            scrollback_max,
            scrollback_len: 0,
            head: 0,
            scroll_top: 0,
            scroll_bot: rows.saturating_sub(1),
            // A freshly allocated buffer is genuinely all-blank (`vec!` of
            // `Cell::default()`), so the first clear can be elided too.
            all_blank: true,
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

    /// Number of scrollback lines currently stored above the visible area.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback_len
    }

    /// Get a cell by **absolute buffer row** (`0` = oldest scrollback row,
    /// `scrollback_len` = top visible row). Used by selection extraction,
    /// which operates across the whole `[scrollback | visible]` buffer.
    #[must_use]
    pub fn abs_cell(&self, col: u16, abs_row: usize) -> Cell {
        let idx = (self.head + abs_row) * usize::from(self.cols) + usize::from(col);
        self.buf.get(idx).copied().unwrap_or_default()
    }

    /// Whether the row at absolute index `abs_row` soft-wraps onto the next.
    #[must_use]
    pub fn abs_wrapped(&self, abs_row: usize) -> bool {
        self.wrapped
            .get(self.head + abs_row)
            .copied()
            .unwrap_or(false)
    }

    /// Get a cell `(col, row)` as seen through a viewport offset.
    ///
    /// `viewport_offset == 0` is the live view (the bottom of the buffer).
    /// `viewport_offset == k` scrolls the whole view up `k` lines into
    /// scrollback: physical row 0 shows scrollback line
    /// `scrollback_len - k`. Rows above the top of history read as blank.
    #[must_use]
    pub fn get_viewport_cell(&self, col: u16, row: u16, viewport_offset: usize) -> Cell {
        // Index into the linear [scrollback | visible] buffer by row.
        // `abs_row` is the absolute physical row: the view's row 0 sits at
        // absolute row `scrollback_len - viewport_offset`.
        // scrollback_len/viewport_offset are bounded by SCROLLBACK_MAX (a
        // small constant, far under i64::MAX), so these always fit.
        let sb = i64::try_from(self.scrollback_len).unwrap_or(i64::MAX);
        let viewport_offset = i64::try_from(viewport_offset).unwrap_or(i64::MAX);
        let abs_row = sb - viewport_offset + i64::from(row);
        if abs_row < 0 {
            return Cell::default();
        }
        let idx = (self.head + abs_row as usize) * usize::from(self.cols) + usize::from(col);
        self.buf.get(idx).copied().unwrap_or_default()
    }

    /// Set the cell at `(col, row)` in the visible area.
    pub fn set_cell(&mut self, col: u16, row: u16, cell: Cell) {
        let idx = self.visible_idx(col, row);
        self.buf[idx] = cell;
        self.all_blank = false;
    }

    /// Mark (or clear) the soft-wrap continuation flag for a visible row.
    pub fn set_wrapped(&mut self, row: u16, wrapped: bool) {
        let idx = self.head + self.scrollback_len + usize::from(row);
        if idx < self.wrapped.len() {
            self.wrapped[idx] = wrapped;
        }
        if wrapped {
            self.all_blank = false;
        }
    }

    /// Whether a visible row soft-wraps onto the next.
    #[must_use]
    pub fn is_wrapped(&self, row: u16) -> bool {
        let idx = self.head + self.scrollback_len + usize::from(row);
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
        // Already provably blank -> the fill + wrapped-reset below are pure
        // memset traffic over cells that are already `Cell::default()`. Skip.
        if self.all_blank {
            return;
        }
        let start = (self.head + self.scrollback_len) * usize::from(self.cols);
        let end =
            (self.head + self.scrollback_len + usize::from(self.rows)) * usize::from(self.cols);
        self.buf[start..end].fill(Cell::default());
        for w in &mut self.wrapped[self.head + self.scrollback_len
            ..self.head + self.scrollback_len + usize::from(self.rows)]
        {
            *w = false;
        }
        self.all_blank = true;
    }

    /// Discard all scrollback history. The visible area keeps its content and
    /// stays in place; the stored rows above it are simply forgotten (the
    /// visible rows are copied down over the scrollback region). Used by the
    /// ED 3 (`CSI 3 J`) "erase scrollback" sequence — and, per this terminal's
    /// chosen behavior, by ED 2 / `clear` as well.
    pub fn erase_scrollback(&mut self) {
        if self.scrollback_len == 0 {
            return;
        }
        let cols = usize::from(self.cols);
        let vis_start = self.head + self.scrollback_len;
        let vis_cells = usize::from(self.rows) * cols;
        // Move the visible block to the front of the buffer and re-anchor the
        // window at head 0.
        self.buf
            .copy_within(vis_start * cols..vis_start * cols + vis_cells, 0);
        self.wrapped
            .copy_within(vis_start..vis_start + usize::from(self.rows), 0);
        // Blank everything after the visible block (old scrollback + slack).
        self.buf[vis_cells..].fill(Cell::default());
        for w in &mut self.wrapped[usize::from(self.rows)..] {
            *w = false;
        }
        self.head = 0;
        self.scrollback_len = 0;
    }

    /// Scroll the region `[top, bot]` up by `n` lines, pulling blank lines at
    /// bottom, for a genuine "content advanced past the bottom margin" event
    /// (linefeed past the last row, or an explicit `CSI Ps S` Scroll Up).
    /// Lines that scroll off the top are pushed into scrollback (primary
    /// screen only, and only when the region starts at row 0).
    ///
    /// Returns the number of lines that entered scrollback — the net upward
    /// shift of on-screen content, which the grid uses to keep selections
    /// glued to their text.
    pub fn scroll_up(&mut self, n: u16, top: u16, bot: u16) -> u16 {
        self.scroll_up_impl(n, top, bot, true)
    }

    /// Delete `n` lines at `row` (DL, `CSI Ps M`): pulls lines below up into
    /// the gap. Unlike a genuine scroll, DL is a line-editing operation, not
    /// output advancing past a margin -- the deleted lines must simply
    /// disappear, never get appended to scrollback (xterm/VTE/kitty all
    /// agree DL/IL never touch history).
    pub fn delete_lines_no_scrollback(&mut self, row: u16, n: u16, bot: u16) {
        self.scroll_up_impl(n, row, bot, false);
    }

    fn scroll_up_impl(&mut self, n: u16, top: u16, bot: u16, push_scrollback: bool) -> u16 {
        let cols = usize::from(self.cols);
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);

        if n == 0 || top_i >= bot_i {
            return 0;
        }
        // Never scroll by more than the region height.
        let n = n.min(bot_i - top_i);

        // Does this scroll rotate content through scrollback history? Only a
        // top-region (top_i == 0) scroll on the primary screen (scrollback_max
        // > 0) does; DL and the alternate screen pass push_scrollback == false
        // and never touch history. NOTE: this must NOT be gated on there being
        // *room to grow* history -- once scrollback is full the scroll still
        // rotates (dropping the oldest line to make room for the new one). The
        // old code selected the branch on `push > 0`, which became 0 when full,
        // so a full-scrollback flood silently discarded every scrolled-off line
        // instead of rotating it in -- history froze at its fill point and the
        // rest of the output was lost.
        let rotates_history = push_scrollback && top_i == 0 && self.scrollback_max > 0;

        let vis_rows = bot_i - top_i + 1;
        let keep = vis_rows - n;
        let base = self.scrollback_len;

        if rotates_history {
            // History rotation is O(n), not O(scrollback). The top `n` visible
            // rows become scrollback simply by GROWING the boundary
            // (scrollback_len += n) -- the rows are already sitting in place, so
            // nothing moves. If that overflows the cap, the oldest `drop_rows`
            // rows are evicted by ADVANCING the floating window head
            // (head += drop_rows); again nothing moves, the old rows just fall
            // outside the window. Only the `n` freshly-exposed bottom rows are
            // written (blanked). `head` may need to walk into slack; compact
            // just before it would run past the buffer end.
            let new_base = (base + n).min(self.scrollback_max);
            let drop_rows = (base + n) - new_base;

            // Ensure the window can grow at the bottom without running off the
            // end of the backing store; compact to the front if not. The
            // highest physical row written is `head + drop_rows + new_base +
            // vis_rows` == `head + base + n + vis_rows` (drop_rows + new_base ==
            // base + n), so reserve that many rows from the current head BEFORE
            // advancing it. (Amortized O(1): compaction fires only once per full
            // traversal of the slack region.)
            self.ensure_tail_room(base + n + vis_rows);

            self.head += drop_rows;
            self.scrollback_len = new_base;

            // Blank the `n` freshly-exposed rows at the bottom of the window.
            let out_rows = new_base + vis_rows; // window rows in use, from head
            let blank_from = self.head + out_rows - n;
            let blank_to = self.head + out_rows;
            self.buf[blank_from * cols..blank_to * cols].fill(Cell::default());
            for w in &mut self.wrapped[blank_from..blank_to] {
                *w = false;
            }
        } else {
            // No scrollback involvement: shift the surviving rows up within the
            // region in place, then blank the bottom n rows. All indices are
            // relative to the floating window (head + base + top_i).
            let region_top = self.head + base + top_i;
            if keep > 0 {
                self.buf.copy_within(
                    (region_top + n) * cols..(region_top + n + keep) * cols,
                    region_top * cols,
                );
                self.wrapped
                    .copy_within(region_top + n..region_top + n + keep, region_top);
            }
            let blank_start = (region_top + keep) * cols;
            self.buf[blank_start..blank_start + n * cols].fill(Cell::default());
            for w in &mut self.wrapped[region_top + keep..region_top + keep + n] {
                *w = false;
            }
        }

        // Net upward shift of on-screen content: the rows that scrolled off
        // the top into history. For a genuine top-region scroll that's simply
        // `n` — the whole visible region moved up by n rows, regardless of
        // whether the scrollback buffer had room to retain them (push) or had
        // to drop the oldest (drop_rows). DL/IL never touch scrollback and so
        // correctly report 0.
        if push_scrollback && top_i == 0 && self.scrollback_max > 0 {
            n.min(u16::MAX as usize) as u16
        } else {
            0
        }
    }

    /// Scroll the region `[top, bot]` down by `n` lines, pulling blank lines at top.
    pub fn scroll_down(&mut self, n: u16, top: u16, bot: u16) {
        let cols = usize::from(self.cols);
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);
        let vis_off = self.head + self.scrollback_len;

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
        // All source reads below are absolute logical rows [0 .. total_old);
        // translate to physical rows through the floating window head.
        let head = self.head;

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
                let base = (head + r) * old_cols;
                if r == cursor_abs {
                    cursor_line = li;
                    cursor_offset = cells.len() + cursor_col;
                }
                cells.extend_from_slice(&self.buf[base..base + old_cols]);
                let cont = self.wrapped.get(head + r).copied().unwrap_or(false);
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

        let total = Self::cap_rows(new_rows_u, self.scrollback_max);
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
        // The rebuilt buffer packs the window at the front: re-anchor head.
        self.head = 0;
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
            let start = (self.head + self.scrollback_len + row) * cols;
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

    /// Guarantee the window `[head .. head + window_rows)` fits within `buf`
    /// with `head` unchanged, or slide the whole live window back to physical
    /// row 0 (a compaction) if it would run off the end. `window_rows` is the
    /// number of rows the window will occupy AFTER the pending growth.
    ///
    /// Compaction copies at most `window_rows` rows, and only fires once the
    /// floating `head` has walked across the entire slack region, so the cost
    /// is amortized to O(1) row-copies per evicted line -- the whole point of
    /// the floating window over the old slide-to-front (which paid O(window)
    /// on every single evicted line and stalled sustained floods).
    fn ensure_tail_room(&mut self, window_rows: usize) {
        let cols = usize::from(self.cols);
        let cap = self.wrapped.len();
        if self.head + window_rows <= cap {
            return;
        }
        // Move the currently-live rows [head .. head + live) down to row 0.
        // Only the rows already in use need to travel; the pending growth is
        // blanked by the caller afterward.
        let live = self.scrollback_len + usize::from(self.rows);
        if self.head > 0 {
            self.buf
                .copy_within(self.head * cols..(self.head + live) * cols, 0);
            self.wrapped.copy_within(self.head..self.head + live, 0);
        }
        self.head = 0;
    }

    fn visible_row_start(&self, row: u16) -> usize {
        (self.head + self.scrollback_len + usize::from(row)) * usize::from(self.cols)
    }

    fn visible_idx(&self, col: u16, row: u16) -> usize {
        self.visible_row_start(row) + usize::from(col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_row(s: &mut Screen, row: u16, ch: char) {
        for c in 0..s.cols() {
            s.set_cell(
                c,
                row,
                Cell {
                    ch,
                    ..Cell::default()
                },
            );
        }
    }

    fn row_char(s: &Screen, row: u16) -> char {
        s.get_cell(0, row).ch
    }

    #[test]
    fn scroll_up_rotates_visible_window_over_scrollback() {
        // cols=2 rows=3, scrollback_max=4.
        let mut s = Screen::new(2, 3, 4);
        fill_row(&mut s, 0, 'a');
        fill_row(&mut s, 1, 'b');
        fill_row(&mut s, 2, 'c');

        // Scroll once: 'a' -> scrollback, visible becomes [b, c, blank].
        s.scroll_up(1, 0, 2);
        assert_eq!(row_char(&s, 0), 'b');
        assert_eq!(row_char(&s, 1), 'c');
        assert_eq!(row_char(&s, 2), ' ');

        // Fill bottom and scroll again: 'b' -> scrollback, visible [c, d, blank].
        fill_row(&mut s, 2, 'd');
        s.scroll_up(1, 0, 2);
        assert_eq!(row_char(&s, 0), 'c', "c must survive, not be lost");
        assert_eq!(row_char(&s, 1), 'd');
        assert_eq!(row_char(&s, 2), ' ');

        fill_row(&mut s, 2, 'e');
        s.scroll_up(1, 0, 2);
        assert_eq!(row_char(&s, 0), 'd');
        assert_eq!(row_char(&s, 1), 'e');
        assert_eq!(row_char(&s, 2), ' ');
    }

    #[test]
    fn scroll_up_accumulates_history_up_to_scrollback_max() {
        // cols=1, rows=2, scrollback_max=3. Each single-line scroll pushes the
        // current top row into history until it saturates at scrollback_max.
        // Exercises the (common, still-growing) drop_rows==0 fast path of the
        // in-place rewrite -- which must copy nothing yet still land the same
        // [oldest..newest] history the old allocate-and-rebuild produced.
        let mut s = Screen::new(1, 2, 3);
        for ch in ['A', 'B', 'C'] {
            fill_row(&mut s, 0, ch);
            s.scroll_up(1, 0, 1);
        }
        assert_eq!(s.scrollback_len(), 3, "history filled to the cap");
        assert_eq!(s.abs_cell(0, 0).ch, 'A', "oldest history row");
        assert_eq!(s.abs_cell(0, 1).ch, 'B');
        assert_eq!(s.abs_cell(0, 2).ch, 'C', "newest history row");
    }

    #[test]
    fn multi_line_scroll_evicts_oldest_when_it_overflows_scrollback() {
        // Drive the drop_rows>0 eviction branch: seed one history row, then a
        // multi-line scroll whose push exceeds the remaining cap, forcing the
        // oldest row to slide off the front. Guards the in-place copy_within +
        // blank arithmetic that replaced the allocate-and-rebuild.
        let mut s = Screen::new(1, 3, 2);
        // First push: history = [p]; visible becomes [q, r, blank].
        fill_row(&mut s, 0, 'p');
        fill_row(&mut s, 1, 'q');
        fill_row(&mut s, 2, 'r');
        s.scroll_up(1, 0, 2);
        assert_eq!(s.scrollback_len(), 1);
        assert_eq!(s.abs_cell(0, 0).ch, 'p');

        // Now a 2-line scroll: q,r would enter history (base=1, n=2), but
        // scrollback_max=2, so the oldest ('p') is evicted -- history = [q, r].
        s.scroll_up(2, 0, 2);
        assert_eq!(s.scrollback_len(), 2, "history capped at max");
        assert_eq!(s.abs_cell(0, 0).ch, 'q', "'p' evicted, 'q' now oldest");
        assert_eq!(s.abs_cell(0, 1).ch, 'r', "newest history row");
        assert_eq!(
            row_char(&s, 0),
            ' ',
            "visible fully blanked by the 2-scroll"
        );
    }

    #[test]
    fn sustained_single_line_scroll_past_full_keeps_rotating_history() {
        // Regression: a flood scrolls one line at a time far past the point
        // where scrollback is FULL. Once len == max, `push = n.min(max-len)`
        // was 0, so the scroll fell into the no-history branch and silently
        // discarded every further scrolled-off line -- history froze at its
        // fill point and later output was lost (scroll-back showed 0..cap then
        // jumped straight to the last screen). Each further scroll must keep
        // rotating: drop the oldest history row, append the newest.
        // cols=1, rows=2, scrollback_max=3.
        let mut s = Screen::new(1, 2, 3);
        // Push A,B,C,D,E,F one line at a time (six scrolls, well past the cap).
        for ch in ['A', 'B', 'C', 'D', 'E', 'F'] {
            fill_row(&mut s, 0, ch);
            s.scroll_up(1, 0, 1);
        }
        // History stays capped at 3 and holds the THREE MOST RECENT scrolled
        // rows -- D,E,F -- not the first three (A,B,C) it would freeze on.
        assert_eq!(s.scrollback_len(), 3, "history stays capped at max");
        assert_eq!(s.abs_cell(0, 0).ch, 'D', "oldest retained row rotated to D");
        assert_eq!(s.abs_cell(0, 1).ch, 'E');
        assert_eq!(s.abs_cell(0, 2).ch, 'F', "newest scrolled row retained");
    }

    #[test]
    fn long_flood_past_slack_compacts_and_keeps_last_lines_correct() {
        // Drive far more scrolls than the backing store's slack can hold, so
        // the floating window head walks off the end and `ensure_tail_room`
        // compacts (potentially several times). Throughout, scrollback must
        // stay capped and hold the most-recent lines in order -- i.e. the
        // floating-window + compaction bookkeeping is transparent. This is the
        // sustained-flood case that must stay O(1)/line (no slide-to-front)
        // AND lossless.
        // cols=1, rows=2, scrollback_max=4 -> cap_rows = 2*(2+4)=12 physical
        // rows; 500 single-line scrolls force many head advances + compactions.
        let mut s = Screen::new(1, 2, 4);
        let n = 500u32;
        for i in 0..n {
            // Distinct glyph per line so misordering/loss is visible.
            let ch = char::from(b'a' + (i % 26) as u8);
            fill_row(&mut s, 0, ch);
            s.scroll_up(1, 0, 1);
        }
        // Each filled row is pushed straight into scrollback by its scroll, so
        // after the flood the visible area is blank and history holds the last
        // `scrollback_max` filled lines (n-4 .. n-1) in order, oldest first.
        assert_eq!(s.scrollback_len(), 4, "history capped at max throughout");
        for k in 0..4u32 {
            let line = n - 4 + k; // lines n-4 .. n-1
            let want = char::from(b'a' + (line % 26) as u8);
            assert_eq!(
                s.abs_cell(0, k as usize).ch,
                want,
                "absolute row {k} should hold flood line {line}"
            );
        }
        assert_eq!(row_char(&s, 0), ' ', "top visible row blank after flood");
        assert_eq!(row_char(&s, 1), ' ', "bottom visible row blank after flood");
    }
}
