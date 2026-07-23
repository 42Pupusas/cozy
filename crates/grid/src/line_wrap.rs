//! Word-aware line breaking for reflow.
//!
//! When [`crate::Screen::resize`] changes the terminal width it must re-break
//! every soft-wrapped logical line to the new column count. Doing that with a
//! naive fixed-stride slice (`line[p * width .. (p + 1) * width]`) chops words
//! in half — "hello world" reflowed to width 8 becomes "hello wo" / "rld".
//! [`LineWrapper`] instead prefers to break at whitespace so a word travels to
//! the next row intact, falling back to a hard break only when a single word is
//! itself wider than the target (there is nowhere clean to go).

use crate::Cell;

/// Breaks one logical line (a flat run of cells) into physical-row pieces at a
/// given column width, preferring whitespace boundaries.
///
/// A wide glyph occupies two adjacent cells (a lead + a spacer), so counting
/// cells already counts columns — no separate width measurement is needed here.
pub struct LineWrapper<'a> {
    line: &'a [Cell],
    /// Target row width in columns/cells (clamped to at least 1).
    width: usize,
}

impl<'a> LineWrapper<'a> {
    /// Wrap `line` to `width` columns. A `width` of 0 is clamped to 1 so the
    /// loop always makes progress.
    pub fn new(line: &'a [Cell], width: usize) -> Self {
        Self {
            line,
            width: width.max(1),
        }
    }

    /// The break points as half-open `[start, end)` cell ranges that partition
    /// the line: `line[r0.0..r0.1]`, `line[r1.0..r1.1]`, … concatenated give
    /// the whole line back, and every piece is at most `width` cells wide. An
    /// empty line yields a single empty piece `(0, 0)` (a blank row still
    /// occupies one physical row).
    pub fn pieces(&self) -> Vec<(usize, usize)> {
        let len = self.line.len();
        if len == 0 {
            return vec![(0, 0)];
        }
        let mut out = Vec::new();
        let mut start = 0usize;
        while start < len {
            // Longest slice that still fits the row.
            let hard_end = (start + self.width).min(len);
            if hard_end == len {
                out.push((start, len));
                break;
            }
            let mut brk = self.clean_break(start, hard_end);
            // Never sever a wide-glyph pair: if the break would land between a
            // lead and its spacer, pull it back one cell so the pair stays
            // together (only when that still leaves at least one cell on this
            // row, otherwise we'd make no progress).
            if brk < len && self.line[brk].spacer() && brk > start + 1 {
                brk -= 1;
            }
            out.push((start, brk));
            start = brk;
        }
        out
    }

    /// Break `line` at the raw column edge every `width` cells — the classic
    /// character-wrap a terminal's autowrap performs, with no whitespace
    /// preference. Wide-glyph pairs are still kept intact (a break that would
    /// land on a spacer is pulled back one cell).
    ///
    /// This exists for the ONE logical line a resize must NOT word-wrap: the
    /// line holding the cursor (the shell's active prompt + input). A shell
    /// redraws that line on SIGWINCH from its own layout model, which assumes
    /// character wrapping; if our reflow broke it earlier (at a space), the
    /// post-reflow cursor row/col would diverge from the shell's and its
    /// clear-and-reprint would land offset, stranding the old wrapped
    /// fragment above as a phantom copy. Char-wrapping the active line keeps
    /// its geometry identical to the shell's, so the redraw overwrites
    /// cleanly. Historical lines have no such redraw and stay word-wrapped
    /// via [`Self::pieces`].
    pub fn hard_pieces(&self) -> Vec<(usize, usize)> {
        let len = self.line.len();
        if len == 0 {
            return vec![(0, 0)];
        }
        let mut out = Vec::new();
        let mut start = 0usize;
        while start < len {
            let mut brk = (start + self.width).min(len);
            // Never sever a wide-glyph pair (same guard as `pieces`).
            if brk < len && self.line[brk].spacer() && brk > start + 1 {
                brk -= 1;
            }
            out.push((start, brk));
            start = brk;
        }
        out
    }

    /// Choose where to end a piece that starts at `start` and can reach at most
    /// `hard_end`. Prefer the position just after the last whitespace cell in
    /// the window (so the piece keeps the trailing space and the next row opens
    /// on a word); if the window holds no whitespace it is a single
    /// over-long word, so hard-break at `hard_end`.
    fn clean_break(&self, start: usize, hard_end: usize) -> usize {
        // Scan back for the last whitespace within (start, hard_end]; break
        // immediately after it. Skipping `start` itself guarantees progress.
        for b in (start + 1..=hard_end).rev() {
            if self.line[b - 1].ch.is_whitespace() {
                return b;
            }
        }
        hard_end
    }
}

#[cfg(test)]
mod tests {
    use super::LineWrapper;
    use crate::Cell;

    /// Build a logical line of single-width cells from `s`.
    fn line(s: &str) -> Vec<Cell> {
        s.chars()
            .map(|c| Cell::new(c, crate::Attrs::default(), 0))
            .collect()
    }

    /// Render the wrapped pieces back to strings for easy assertions.
    fn wrap(s: &str, width: usize) -> Vec<String> {
        let cells = line(s);
        LineWrapper::new(&cells, width)
            .pieces()
            .into_iter()
            .map(|(a, b)| cells[a..b].iter().map(|c| c.ch).collect())
            .collect()
    }

    #[test]
    fn breaks_at_word_boundary_not_mid_word() {
        assert_eq!(wrap("hello world", 8), vec!["hello ", "world"]);
    }

    #[test]
    fn hard_breaks_a_word_longer_than_width() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn mixes_long_word_then_short_words() {
        assert_eq!(wrap("abcdefgh ij", 5), vec!["abcde", "fgh ", "ij"]);
    }

    #[test]
    fn line_that_fits_is_one_piece() {
        assert_eq!(wrap("short", 10), vec!["short"]);
    }

    #[test]
    fn empty_line_is_one_empty_piece() {
        assert_eq!(LineWrapper::new(&[], 10).pieces(), vec![(0, 0)]);
    }

    #[test]
    fn pieces_partition_the_line_exactly() {
        let cells = line("the quick brown fox jumps");
        let pieces = LineWrapper::new(&cells, 7).pieces();
        // Contiguous, non-overlapping, covering [0, len).
        assert_eq!(pieces.first().unwrap().0, 0);
        assert_eq!(pieces.last().unwrap().1, cells.len());
        for w in pieces.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        // No piece exceeds the width.
        assert!(pieces.iter().all(|&(a, b)| b - a <= 7));
    }

    /// Render `hard_pieces` back to strings for easy assertions.
    fn hard_wrap(s: &str, width: usize) -> Vec<String> {
        let cells = line(s);
        LineWrapper::new(&cells, width)
            .hard_pieces()
            .into_iter()
            .map(|(a, b)| cells[a..b].iter().map(|c| c.ch).collect())
            .collect()
    }

    #[test]
    fn hard_pieces_break_at_the_column_edge_ignoring_words() {
        // Unlike `pieces`, the character-wrap must split at the raw width,
        // mid-word, matching a terminal's autowrap and the shell's own model.
        assert_eq!(hard_wrap("hello world", 8), vec!["hello wo", "rld"]);
    }

    #[test]
    fn hard_pieces_empty_line_is_one_empty_piece() {
        assert_eq!(LineWrapper::new(&[], 10).hard_pieces(), vec![(0, 0)]);
    }

    #[test]
    fn hard_pieces_keep_wide_pair_together() {
        // The wide-glyph guard applies to the character-wrap path too.
        let mut cells = line("ab");
        cells.push(Cell::wide_lead('世', crate::Attrs::default(), 0));
        cells.push(Cell::wide_spacer(crate::Attrs::default(), 0));
        cells.extend(line("cd"));
        let pieces = LineWrapper::new(&cells, 3).hard_pieces();
        assert!(pieces.iter().all(|&(_, b)| b != 3));
    }

    #[test]
    fn keeps_wide_pair_together() {
        // A wide glyph: lead + spacer. Width 3 would naively split the pair at
        // index 3 (spacer), but the wrapper pulls the break back.
        let mut cells = line("ab");
        cells.push(Cell::wide_lead('世', crate::Attrs::default(), 0));
        cells.push(Cell::wide_spacer(crate::Attrs::default(), 0));
        cells.extend(line("cd"));
        // cells: a b 世 <sp> c d  (indices 0..6)
        let pieces = LineWrapper::new(&cells, 3).pieces();
        // The first break must not fall at index 3 (between lead and spacer).
        assert!(pieces.iter().all(|&(_, b)| b != 3));
    }
}
