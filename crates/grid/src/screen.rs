#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::missing_const_for_fn,
    clippy::must_use_candidate
)]
//! 2-D cell buffer with scrollback.

use crate::{Cell, cell_store::CellStore};

/// A rectangular grid of cells with optional scrollback.
///
/// The buffer is a **ring at row granularity**: the logical
/// `[scrollback | visible]` window starts at physical row `head` and wraps
/// modulo the buffer height ([`Self::phys_row`]). This is what makes a
/// sustained flood cheap: once scrollback is full, evicting the oldest line
/// is just `head += 1 (mod total)` -- rows NEVER move. An earlier design kept
/// the window physically contiguous by letting `head` float through slack and
/// compacting (an O(window) memmove) when it ran off the end; stats showed
/// those compactions moving ~28x the input volume on a flood, so the ring
/// removed them outright (and halved the allocation -- no slack needed).
///
/// Rows never straddle the wrap seam (wrapping is per whole row), so any
/// single row is always one contiguous slice; only multi-row block moves
/// (region scrolls) walk row-by-row through [`Self::copy_row`].
pub struct Screen {
    /// Backing store, `cap_rows(rows, scrollback_max)` rows tall. The live
    /// window occupies the `scrollback_len + rows` physical rows starting at
    /// `head`, wrapping modulo the buffer height.
    buf: CellStore,
    /// Per physical row: true if the line continues (soft-wrapped) onto the
    /// next physical row. Parallel to rows in `buf`; same length as `buf/cols`.
    /// This is what lets `resize` reflow instead of clip.
    wrapped: Vec<bool>,
    /// Per physical row: true if the row is *logically blank* but its `buf`
    /// cells were never actually cleared -- they hold stale garbage that must
    /// read as [`Cell::default()`]. This is the scroll fast path: exposing a
    /// fresh row at the bottom of a scroll used to memset `cols` cells
    /// (gigabytes of pure blanking traffic on a sustained flood, ~half the
    /// measured memory volume); now it sets one bit, and the blank is
    /// materialized lazily -- by the next write to the row (which blanks only
    /// the cells *around* what it writes, i.e. nothing at all for a
    /// full-width line) or read through the accessors. INVARIANT: every path
    /// that moves rows (`copy_within` on `buf`/`wrapped`) must move these
    /// bits identically, and every direct `buf` read must consult them --
    /// all reads are funnelled through `get_cell`/`abs_cell`/
    /// `get_viewport_cell`/`row_is_blank`, and `resize` materializes
    /// everything up front rather than sprinkling checks through reflow.
    blank_rows: Vec<bool>,
    cols: u16,
    rows: u16,
    /// Maximum scrollback lines (0 for alternate screen).
    scrollback_max: usize,
    /// Current number of scrollback lines stored.
    scrollback_len: usize,
    /// Physical row where the window (absolute row 0 = oldest scrollback row)
    /// currently begins. Advanced (mod buffer height) on eviction; reset to 0
    /// only when the buffer is rebuilt (`resize`).
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
    /// Scroll-path work counters for the benchmark harness. Present only
    /// under the `stats` feature so the default build carries no dead state;
    /// all recording goes through the `note_*` wrappers below, which have
    /// no-op twins when the feature is off.
    #[cfg(feature = "stats")]
    stats: crate::ScrollStats,
}

impl Screen {
    /// Total rows to allocate: exactly the biggest possible window
    /// (`rows + scrollback_max`). The buffer is a ring, so no slack region is
    /// needed -- eviction advances `head` modulo this total and rows never
    /// move. (The pre-ring design allocated 2x this so a floating head could
    /// walk through slack between compactions; the ring removed both the
    /// compactions and the extra half.)
    ///
    /// Was a free `cap_rows(rows, scrollback_max)` function -- moved onto
    /// `Screen` as an associated function since it computes a `Screen`
    /// sizing invariant, not a standalone calculation.
    fn cap_rows(rows: usize, scrollback_max: usize) -> usize {
        rows + scrollback_max
    }

    /// Total physical rows in the backing store (the ring's modulus).
    fn total_rows(&self) -> usize {
        self.wrapped.len()
    }

    /// Map an absolute window row (0 = oldest scrollback row,
    /// `scrollback_len` = top visible row) to its physical row in the ring.
    ///
    /// `head < total` and `abs <= total` always hold, so a single conditional
    /// subtract replaces the general modulo -- this sits on every access path,
    /// and a hardware divide here would be pure waste.
    #[inline]
    fn phys_row(&self, abs: usize) -> usize {
        let total = self.total_rows();
        let p = self.head + abs;
        if p >= total { p - total } else { p }
    }

    pub fn new(cols: u16, rows: u16, scrollback_max: usize) -> Self {
        let total = Self::cap_rows(usize::from(rows), scrollback_max);
        Self {
            buf: CellStore::new(total * usize::from(cols)),
            wrapped: vec![false; total],
            // A fresh buffer is genuinely blank, so no rows are *lazily*
            // blank -- the bits track "buf content is stale garbage", which
            // is false everywhere at construction.
            blank_rows: vec![false; total],
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
            #[cfg(feature = "stats")]
            stats: crate::ScrollStats::default(),
        }
    }

    /// Read the accumulated scroll-path counters (benchmark harness only).
    #[cfg(feature = "stats")]
    pub fn scroll_stats(&self) -> crate::ScrollStats {
        self.stats
    }

    /// Zero the scroll-path counters (benchmark harness only).
    #[cfg(feature = "stats")]
    pub fn reset_scroll_stats(&mut self) {
        self.stats.reset();
    }

    /// Record one scroll that blanked `rows` rows -- no-op without `stats`,
    /// so the hot path stays free of scattered `#[cfg]` blocks.
    #[cfg(feature = "stats")]
    fn note_scroll(&mut self, rows: usize) {
        self.stats.note_scroll(rows as u64);
    }

    #[cfg(not(feature = "stats"))]
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "signature must match the stats twin"
    )]
    fn note_scroll(&mut self, _rows: usize) {}

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
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        if self.blank_rows[phys] {
            return Cell::default();
        }
        self.buf
            .cell(phys * usize::from(self.cols) + usize::from(col))
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
        // Rows past the live window don't exist -- with a ring, an
        // unchecked mapping would alias back onto live rows, so bound it
        // here rather than relying on out-of-range indexing to fail.
        if abs_row >= self.scrollback_len + usize::from(self.rows) {
            return Cell::default();
        }
        let phys = self.phys_row(abs_row);
        if self.blank_rows[phys] {
            return Cell::default();
        }
        let idx = phys * usize::from(self.cols) + usize::from(col);
        if idx < self.buf.len() {
            self.buf.cell(idx)
        } else {
            Cell::default()
        }
    }

    /// Whether the row at absolute index `abs_row` soft-wraps onto the next.
    #[must_use]
    pub fn abs_wrapped(&self, abs_row: usize) -> bool {
        if abs_row >= self.scrollback_len + usize::from(self.rows) {
            return false;
        }
        self.wrapped[self.phys_row(abs_row)]
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
        // Delegate to abs_cell: it owns the window bound + ring mapping +
        // blank-bit consultation.
        self.abs_cell(col, abs_row as usize)
    }

    /// Set the cell at `(col, row)` in the visible area.
    pub fn set_cell(&mut self, col: u16, row: u16, cell: Cell) {
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        self.materialize_row(phys);
        self.buf
            .set_cell(phys * usize::from(self.cols) + usize::from(col), cell);
        self.all_blank = false;
    }

    /// Write both halves of a wide-glyph pair (or one narrow cell when
    /// `spacer` is `None`) with a single physical-row resolution.
    ///
    /// `Grid::print` writes one or two adjacent cells for nearly every
    /// printable event; resolving the lazy-blank bit and the row base once
    /// here keeps that hot path free of repeated index arithmetic while
    /// retaining `Screen` ownership of the backing-store invariants.
    pub(crate) fn set_cell_pair(&mut self, row: u16, col: u16, lead: Cell, spacer: Option<Cell>) {
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        self.materialize_row(phys);
        let base = phys * usize::from(self.cols) + usize::from(col);
        self.buf.set_cell(base, lead);
        if let Some(sp) = spacer {
            self.buf.set_cell(base + 1, sp);
        }
        self.all_blank = false;
    }

    /// If physical row `phys` is lazily blank, actually write the blanks its
    /// bit has been standing in for, and clear the bit. Called before any
    /// partial write into the row; whole-row writers clear the bit
    /// themselves after overwriting everything.
    #[inline]
    fn materialize_row(&mut self, phys: usize) {
        if self.blank_rows[phys] {
            self.blank_rows[phys] = false;
            let cols = usize::from(self.cols);
            self.buf.clear_range(phys * cols..(phys + 1) * cols);
        }
    }

    /// Mark physical rows `[from, to)` as lazily blank and clear their
    /// soft-wrap flags -- the O(rows) replacement for the O(rows*cols)
    /// memset the scroll paths used to pay per exposed row.
    ///
    /// Physical-row range, no wrap handling: callers pass ranges derived
    /// from a single row or an already-unwrapped span.
    #[inline]
    fn mark_rows_blank(&mut self, from: usize, to: usize) {
        for b in &mut self.blank_rows[from..to] {
            *b = true;
        }
        for w in &mut self.wrapped[from..to] {
            *w = false;
        }
    }

    /// Mark `count` window rows starting at absolute row `abs` lazily blank,
    /// following the ring wrap. The ring counterpart of [`Self::mark_rows_blank`]
    /// for callers that speak absolute window rows.
    #[inline]
    fn mark_abs_rows_blank(&mut self, abs: usize, count: usize) {
        if count == 0 {
            return;
        }
        let total = self.total_rows();
        let start = self.phys_row(abs);
        if count == 1 {
            // A one-row scroll is the terminal flood hot path. Avoid forming
            // two slices and running two iterator loops for the common case;
            // the ring mapping already gives us the exact physical row.
            self.blank_rows[start] = true;
            self.wrapped[start] = false;
            return;
        }
        let first = count.min(total - start);
        self.mark_rows_blank(start, start + first);
        // Wrapped remainder continues at physical row 0.
        self.mark_rows_blank(0, count - first);
    }

    /// Copy one whole window row `src_abs` -> `dst_abs` (cells + wrapped +
    /// blank bit), each independently ring-mapped. Region scrolls and
    /// line edits move blocks row-by-row through this so the ring seam is
    /// handled in exactly one place.
    #[inline]
    fn copy_row(&mut self, src_abs: usize, dst_abs: usize) {
        let cols = usize::from(self.cols);
        let src = self.phys_row(src_abs);
        let dst = self.phys_row(dst_abs);
        if src == dst {
            return;
        }
        self.buf
            .copy_within(src * cols..(src + 1) * cols, dst * cols);
        self.wrapped[dst] = self.wrapped[src];
        // Blank bits travel with their rows: the bit is what makes a
        // lazily-blank row's stale cells read as blank.
        self.blank_rows[dst] = self.blank_rows[src];
    }

    /// Write a contiguous run of narrow printable cells on one visible row.
    ///
    /// The grid owns cursor semantics, while `Screen` owns the row layout and
    /// cell representation. Keeping this tight loop here avoids recomputing a
    /// physical index for every byte in an ASCII flood.
    pub(crate) fn write_ascii_cells(
        &mut self,
        row: u16,
        col: u16,
        bytes: &[u8],
        attrs: crate::Attrs,
        link_id: u32,
    ) -> usize {
        let cols = usize::from(self.cols);
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        let col = usize::from(col);
        let count = bytes.len().min(cols - col);
        // Slice the whole physical row once: every access below (the
        // lazy-blank edge fills and the store loop) is then bounds-check
        // free relative to this slice, and the physical row index is
        // derived by multiplication only -- an earlier version took a
        // precomputed `row_start` and recovered `phys` by division, which
        // perf showed dominating the function. Rows never straddle the
        // ring seam, so the whole row is one contiguous slice.
        let row_start = phys * cols;
        // Lazy-blank handling: the incoming span itself will immediately
        // overwrite its cells. Materialize only its untouched prefix/suffix,
        // not the entire logical row. Typical PTY lines start at column zero
        // and stop before the margin, so this turns an 80-cell clear followed
        // by a 56-cell write into just a 24-cell tail clear. Keeping the
        // blank-row bit until this point still avoids all stores for a
        // full-width write.
        if self.blank_rows[phys] {
            self.blank_rows[phys] = false;
            if col > 0 {
                self.buf.clear_range(row_start..row_start + col);
            }
            let written_end = col + count;
            if written_end < cols {
                self.buf
                    .clear_range(row_start + written_end..row_start + cols);
            }
        }
        // The structure-of-arrays backing store interns the SGR/link style
        // once, then writes compact character/style/placement planes.
        self.buf
            .write_ascii(row_start + col, &bytes[..count], attrs, link_id);
        if count > 0 {
            self.all_blank = false;
        }
        count
    }

    /// Mark (or clear) the soft-wrap continuation flag for a visible row.
    pub fn set_wrapped(&mut self, row: u16, wrapped: bool) {
        let idx = self.phys_row(self.scrollback_len + usize::from(row));
        self.wrapped[idx] = wrapped;
        if wrapped {
            self.all_blank = false;
        }
    }

    /// Whether a visible row soft-wraps onto the next.
    #[must_use]
    pub fn is_wrapped(&self, row: u16) -> bool {
        self.wrapped[self.phys_row(self.scrollback_len + usize::from(row))]
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
        // A full-width erase is exactly what the lazy-blank bit represents:
        // set it and skip the memset (and the wide-pair seam checks below --
        // a blank row has no pairs to orphan). Partial erases materialize
        // first so the untouched cells read correctly afterward.
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        if col0 == 0 && usize::from(col1) >= usize::from(self.cols) - 1 {
            self.mark_rows_blank(phys, phys + 1);
            return;
        }
        self.materialize_row(phys);
        let base = phys * usize::from(self.cols);
        let mut c0 = usize::from(col0);
        let mut c1 = usize::from(col1).min(usize::from(self.cols) - 1);
        // Never orphan half of a wide-char pair: if the erase boundary falls
        // on a spacer cell, pull c0 left onto its lead; if it falls on a wide
        // lead cell, push c1 right onto its spacer.
        if c0 < self.cols as usize && self.buf.cell(base + c0).spacer() && c0 > 0 {
            c0 -= 1;
        }
        if c1 < self.cols as usize && self.buf.cell(base + c1).wide() && c1 + 1 < self.cols as usize
        {
            c1 += 1;
        }
        self.buf.clear_range(base + c0..base + c1 + 1);
        // Erasing through the right margin breaks any soft-wrap continuation.
        if c1 + 1 >= usize::from(self.cols) {
            self.set_wrapped(row, false);
        }
    }

    /// Clear entire visible area.
    pub fn clear(&mut self) {
        // Already provably blank -> nothing to do. Otherwise the whole
        // visible area goes lazily blank: O(rows) bit-sets instead of the
        // O(rows*cols) memset this used to be.
        if self.all_blank {
            return;
        }
        self.mark_abs_rows_blank(self.scrollback_len, usize::from(self.rows));
        self.all_blank = true;
    }

    /// Discard all scrollback history. The visible area keeps its content and
    /// stays in place; the stored rows above it are simply forgotten. Used by
    /// the ED 3 (`CSI 3 J`) "erase scrollback" sequence — and, per this
    /// terminal's chosen behavior, by ED 2 / `clear` as well.
    ///
    /// With the ring this is O(scrollback) *bit-sets* and zero cell traffic:
    /// advance `head` past the forgotten rows (the visible block simply
    /// becomes the whole window) and mark the freed rows lazily blank so
    /// stale history can never be read back. This used to memmove the entire
    /// visible block to the buffer front.
    pub fn erase_scrollback(&mut self) {
        if self.scrollback_len == 0 {
            return;
        }
        let dead = self.scrollback_len;
        self.head = self.phys_row(dead);
        self.scrollback_len = 0;
        // The freed rows now sit at the END of the window's ring span
        // (absolute rows [rows .. rows + dead)); mark them blank so nothing
        // stale is exposed if the window grows back over them.
        self.mark_abs_rows_blank(usize::from(self.rows), dead);
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
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);

        if n == 0 || top_i >= bot_i {
            return 0;
        }
        // Never scroll by more than the region height.
        let n = n.min(bot_i - top_i);
        self.note_scroll(n);

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
            // History rotation is O(n), not O(scrollback) -- and with the
            // ring it is O(n) FOREVER, with no amortized compaction debt.
            // The top `n` visible rows become scrollback simply by GROWING
            // the boundary (scrollback_len += n) -- the rows are already
            // sitting in place, so nothing moves. If that overflows the cap,
            // the oldest `drop_rows` rows are evicted by ADVANCING `head`
            // around the ring; again nothing moves, the evicted rows'
            // storage is exactly where the freshly-exposed bottom rows wrap
            // to. (The pre-ring floating window did the same until `head`
            // ran off the buffer end, then paid an O(window) compaction
            // memmove -- measured at ~28x the input volume on a flood.)
            let new_base = (base + n).min(self.scrollback_max);
            let drop_rows = (base + n) - new_base;

            self.head = self.phys_row(drop_rows);
            self.scrollback_len = new_base;

            // Mark the `n` freshly-exposed rows at the bottom of the window
            // lazily blank. This bit-set replaces what used to be the single
            // biggest memory cost of a sustained flood: memsetting `n * cols`
            // cells on every scroll (~half of all traffic at 512 MiB). The
            // blanks materialize only if something reads or partially writes
            // the row -- a full-width print run never pays for them at all.
            let out_rows = new_base + vis_rows; // window rows in use, from head
            self.mark_abs_rows_blank(out_rows - n, n);
        } else {
            // No scrollback involvement: shift the surviving rows up within
            // the region in place, then blank the bottom n rows. Region rows
            // may straddle the ring seam, so move row-by-row through the
            // seam-aware copy_row (top-to-bottom is the safe direction for
            // an upward shift: each destination is read before overwritten).
            let region_abs = base + top_i;
            for i in 0..keep {
                self.copy_row(region_abs + n + i, region_abs + i);
            }
            self.mark_abs_rows_blank(region_abs + keep, n);
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
        let n = usize::from(n);
        let top_i = usize::from(top);
        let bot_i = usize::from(bot);

        if n == 0 || top_i >= bot_i {
            return;
        }

        let move_rows = (bot_i - top_i + 1).saturating_sub(n);
        // Downward shift: iterate bottom-to-top so each source row is read
        // before it is overwritten. Row-by-row through the seam-aware
        // copy_row -- region rows may straddle the ring's wrap point.
        let region_abs = self.scrollback_len + top_i;
        for i in (0..move_rows).rev() {
            self.copy_row(region_abs + i, region_abs + i + n);
        }
        // Blank top n rows (lazily -- bit-set, no memset).
        let blank_n = n.min(bot_i - top_i + 1);
        self.mark_abs_rows_blank(region_abs, blank_n);
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
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        self.materialize_row(phys);
        let base = phys * cols;
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
        self.buf.clear_range(base + col_i + move_len..base + cols);
        // The shift can slice a wide-char pair in half at either seam.
        self.sanitize_wide_pairs(row);
    }

    /// Insert `n` blank characters at `(col, row)`, shifting right chars right.
    pub fn insert_chars(&mut self, col: u16, row: u16, n: u16) {
        let cols = usize::from(self.cols);
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        self.materialize_row(phys);
        let base = phys * cols;
        let col_i = usize::from(col);
        let n_i = usize::from(n).min(cols - col_i);
        let move_len = cols - col_i - n_i;
        if move_len > 0 {
            self.buf
                .copy_within(base + col_i..base + col_i + move_len, base + col_i + n_i);
        }
        self.buf.clear_range(base + col_i..base + col_i + n_i);
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
            let cell = self.buf.cell(base + col);
            if cell.wide() {
                let has_partner = col + 1 < cols && self.buf.cell(base + col + 1).spacer();
                if has_partner {
                    col += 1; // skip the valid spacer partner
                } else {
                    self.buf.set_cell(base + col, Cell::default());
                }
            } else if cell.spacer() {
                // A spacer with no wide lead directly to its left is orphaned.
                let has_lead = col > 0 && self.buf.cell(base + col - 1).wide();
                if !has_lead {
                    self.buf.set_cell(base + col, Cell::default());
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
        // Reflow below reads raw `buf` row slices; materialize every lazily
        // blank row in the live window first so it reads its actual blanks
        // instead of stale garbage. Resize is rare and already O(window),
        // so paying the deferred memsets here (once) is the right trade.
        for abs in 0..total_old {
            let phys = self.phys_row(abs);
            self.materialize_row(phys);
        }
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
                // Ring-map each absolute row: the window may wrap mid-line.
                let phys = self.phys_row(r);
                let base = phys * old_cols;
                if r == cursor_abs {
                    cursor_line = li;
                    cursor_offset = cells.len() + cursor_col;
                }
                let cont = self.wrapped[phys];
                if cont {
                    // A soft-wrapped (continuation) row that our own word-aware
                    // reflow produced may be RAGGED -- broken before the margin
                    // and padded with blanks -- unlike a terminal-autowrapped
                    // row, which always fills to the last column. Concatenating
                    // that padding would reappear as phantom spaces in the
                    // logical line; and since a word-break's separator space is
                    // byte-identical to a padding blank (`Cell::default()`),
                    // every narrow<->wide reflow cycle would inject an extra
                    // space, growing lines until bottom-anchored repack pushed
                    // visible content off into scrollback (the "resize deletes
                    // lines" bug). So trim this row's trailing blanks: if that
                    // leaves it short of full width the break was a WORD break,
                    // so restore exactly one separator space; a still-full row
                    // was a hard break (an over-long word) and joins seamlessly.
                    let row = self.buf.cells(base..base + old_cols);
                    let mut end = row.len();
                    while end > 0 && row[end - 1] == Cell::default() {
                        end -= 1;
                    }
                    cells.extend_from_slice(&row[..end]);
                    if end < old_cols {
                        cells.push(Cell::default());
                    }
                } else {
                    cells.extend(self.buf.cells(base..base + old_cols));
                }
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
            // Break the logical line at word boundaries where possible, only
            // hard-splitting a word that is itself wider than the row. This is
            // what replaced the old fixed-stride `p * new_cols_u` slice, which
            // chopped words mid-glyph on every reflow.
            //
            // EXCEPT the cursor's own logical line (the shell's active prompt +
            // input): word-wrapping it would move its physical rows/columns
            // away from where the shell — which redraws on SIGWINCH using its
            // own character-wrap layout model — expects the cursor to be. The
            // mismatch makes the shell's clear-and-reprint land offset, leaving
            // the old wrapped fragment stranded above as a phantom copy. So the
            // active line is character-wrapped (`hard_pieces`), matching the
            // shell exactly; committed history stays word-wrapped for looks.
            let wrapper = crate::line_wrap::LineWrapper::new(line, new_cols_u);
            let pieces = if li == cursor_line {
                wrapper.hard_pieces()
            } else {
                wrapper.pieces()
            };
            let piece_count = pieces.len();
            let base_idx = rows_out.len();
            for (p, &(start, end)) in pieces.iter().enumerate() {
                let mut row_cells = vec![Cell::default(); new_cols_u];
                if start < end {
                    row_cells[..end - start].copy_from_slice(&line[start..end]);
                }
                // A word-aware break never lands inside a wide-glyph pair (the
                // wrapper pulls back off a spacer seam), but a *hard* break of
                // an over-long word still can. Blank any orphaned lead (no
                // spacer to its right) or spacer (no lead to its left) rather
                // than let a half-glyph render or corrupt the next row's read.
                if let Some(last) = row_cells.last_mut()
                    && last.wide()
                {
                    *last = Cell::default();
                }
                if let Some(first) = row_cells.first_mut()
                    && first.spacer()
                {
                    *first = Cell::default();
                }
                rows_out.push((row_cells, p + 1 < piece_count));
            }
            if li == cursor_line {
                // Find the piece whose cell range contains the cursor offset.
                // Pieces partition `[0, line.len())`, so a cursor offset at or
                // past the line length sits one position beyond the content.
                let hit = pieces
                    .iter()
                    .position(|&(start, end)| cursor_offset >= start && cursor_offset < end);
                if let Some(p) = hit {
                    let (start, _) = pieces[p];
                    new_cursor_idx = base_idx + p;
                    new_cursor_col = (cursor_offset - start).min(new_cols_u - 1);
                } else {
                    // The cursor sits one position past the line's content (an
                    // eager-wrap terminal parks it at the start of a fresh row
                    // after filling the last column). That row was trimmed as a
                    // trailing blank, so re-materialize it instead of clamping
                    // the cursor back onto the final glyph — clamping is what
                    // made the shell's SIGWINCH repaint clear the wrong rows.
                    let (last_start, last_end) = *pieces.last().unwrap();
                    let last_fits = last_end - last_start < new_cols_u;
                    if last_fits {
                        // Room on the final piece's row: park the cursor just
                        // past its last cell rather than opening a new row.
                        new_cursor_idx = base_idx + piece_count - 1;
                        new_cursor_col = (cursor_offset - last_start).min(new_cols_u - 1);
                    } else {
                        // The last content row is packed full, so the parked
                        // cursor opens a fresh row. Its column is how far the
                        // offset sits past the content end (`last_end`), matching
                        // the old fixed-stride `cursor_offset mod new_cols_u`
                        // when the line length was a multiple of the width.
                        rows_out.push((vec![Cell::default(); new_cols_u], false));
                        new_cursor_idx = rows_out.len() - 1;
                        new_cursor_col = (cursor_offset - last_end).min(new_cols_u - 1);
                    }
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
        let mut new_buf = CellStore::new(total * new_cols_u);
        let mut new_wrapped = vec![false; total];
        // Freshly zeroed buffer: nothing is lazily blank (see `new`).
        let new_blank_rows = vec![false; total];

        for i in 0..new_sb_len {
            let (cells, w) = &rows_out[dropped + i];
            let dst = i * new_cols_u;
            for (offset, cell) in cells.iter().copied().enumerate() {
                new_buf.set_cell(dst + offset, cell);
            }
            new_wrapped[i] = *w;
        }
        for i in 0..visible_count {
            let (cells, w) = &rows_out[overflow + i];
            let dst = (new_sb_len + i) * new_cols_u;
            for (offset, cell) in cells.iter().copied().enumerate() {
                new_buf.set_cell(dst + offset, cell);
            }
            new_wrapped[new_sb_len + i] = *w;
        }

        self.buf = new_buf;
        self.wrapped = new_wrapped;
        self.blank_rows = new_blank_rows;
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
            let phys = self.phys_row(self.scrollback_len + row);
            // A lazily-blank row's cells are stale garbage; the bit says the
            // row is content-free, so trust it and skip the scan.
            if self.blank_rows[phys] {
                continue;
            }
            let start = phys * cols;
            if self
                .buf
                .cells(start..start + cols)
                .iter()
                .any(|c| *c != Cell::default())
            {
                return row as u16 + 1;
            }
        }
        0
    }

    // ── private ───────────────────────────────────────────────────────────

    /// Borrow the visible row's cells as one contiguous slice, or `None`
    /// when the row is lazily blank (its `buf` cells are stale garbage that
    /// must read as [`Cell::default()`]).
    ///
    /// This exists for the snapshot gather path: copying a dirty row used to
    /// go through [`Self::get_cell`] once per column, recomputing the
    /// physical row and re-checking `blank_rows` + bounds for every single
    /// cell. A slice lets the caller do one `extend_from_slice` (a straight
    /// memcpy) instead. The `Option` forces the caller to handle the
    /// blank-row case explicitly rather than ever seeing stale cells.
    #[must_use]
    pub(crate) fn visible_row_cells(&self, row: u16) -> Option<Vec<Cell>> {
        let phys = self.phys_row(self.scrollback_len + usize::from(row));
        if self.blank_rows[phys] {
            return None;
        }
        let start = phys * usize::from(self.cols);
        Some(self.buf.row_cells(start..start + usize::from(self.cols)))
    }

    /// Physical start index (in `buf`) of a visible row, ring-mapped.
    fn visible_row_start(&self, row: u16) -> usize {
        self.phys_row(self.scrollback_len + usize::from(row)) * usize::from(self.cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill_row(s: &mut Screen, row: u16, ch: char) {
        for c in 0..s.cols() {
            s.set_cell(c, row, Cell::new(ch, crate::Attrs::default(), 0));
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
    fn long_flood_wraps_ring_many_times_and_keeps_last_lines_correct() {
        // Drive far more scrolls than the ring can hold, so `head` wraps
        // around the buffer many times over. Throughout, scrollback must
        // stay capped and hold the most-recent lines in order -- i.e. the
        // ring bookkeeping is transparent. This is the sustained-flood case
        // that must stay O(1)/line (no slide-to-front, no compaction)
        // AND lossless.
        // cols=1, rows=2, scrollback_max=4 -> cap_rows = 2+4 = 6 physical
        // rows; 500 single-line scrolls wrap the ring ~80 times.
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
