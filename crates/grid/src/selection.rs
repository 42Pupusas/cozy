//! Text selection model: a range of cells in the buffer (visible or
//! scrollback), plus conversion of that range into clipboard text.
//!
//! A selection is anchored in **absolute buffer rows** — the linear
//! `[scrollback | visible]` coordinate space the [`Screen`] already uses
//! internally — so it stays valid as the terminal scrolls. The renderer /
//! input layer converts pointer positions (viewport rows) into absolute rows
//! via the current `scrollback_len` and `viewport_offset`.
//!
//! Extracted text joins soft-wrapped physical rows back into single logical
//! lines (no newline between a row flagged `wrapped` and its continuation),
//! inserts a real `\n` at hard line breaks, and trims trailing spaces from
//! each physical row — matching what xterm/Alacritty put on the clipboard.

use crate::screen::Screen;

/// One endpoint of a selection, in absolute buffer coordinates.
///
/// `row` indexes the linear buffer: `0` is the oldest scrollback row,
/// `scrollback_len` is the top visible row. `col` is the cell column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub col: u16,
    pub row: usize,
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Point {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.row.cmp(&other.row).then(self.col.cmp(&other.col))
    }
}

/// A contiguous character-cell selection. `start`/`end` are stored unordered
/// (as the user dragged); [`Selection::ordered`] normalizes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
}

impl Selection {
    /// Begin a selection anchored at `point` (both ends equal until the drag
    /// extends one of them).
    #[must_use]
    pub const fn new(point: Point) -> Self {
        Self {
            start: point,
            end: point,
        }
    }

    /// The (inclusive) start and end in buffer order, regardless of drag
    /// direction.
    #[must_use]
    pub fn ordered(&self) -> (Point, Point) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    /// True if the selection spans at least one non-degenerate cell range.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Does this selection include absolute `(row, col)`?
    #[must_use]
    pub fn contains(&self, row: usize, col: u16) -> bool {
        if self.is_empty() {
            return false;
        }
        let (s, e) = self.ordered();
        (row > s.row || (row == s.row && col >= s.col))
            && (row < e.row || (row == e.row && col <= e.col))
    }
}

impl Screen {
    /// Extract the selected text for the clipboard.
    ///
    /// Soft-wrapped rows are rejoined into their logical line (no `\n`); hard
    /// breaks emit a `\n`. Trailing whitespace is trimmed per physical row so
    /// a mid-screen selection doesn't drag a tail of padding spaces along.
    #[must_use]
    pub fn selected_text(&self, sel: &Selection) -> String {
        if sel.is_empty() {
            return String::new();
        }
        let (start, end) = sel.ordered();
        let row_width = usize::from(self.cols());
        let total_rows = self.scrollback_len() + usize::from(self.rows());
        let mut out = String::new();

        for abs_row in start.row..=end.row {
            if abs_row >= total_rows {
                break;
            }
            let first_col = if abs_row == start.row {
                usize::from(start.col)
            } else {
                0
            };
            let last_col = if abs_row == end.row {
                usize::from(end.col)
            } else {
                row_width - 1
            };
            let last_col = last_col.min(row_width - 1);

            // Gather the row's characters (skipping spacer halves of wide
            // glyphs, which carry a placeholder ' ' that would double-count).
            let mut line = String::new();
            for col in first_col..=last_col {
                // Safe: `col` is bounded above by `row_width - 1`, and
                // `row_width` came from `self.cols(): u16`.
                let cell = self.abs_cell(u16::try_from(col).unwrap_or(u16::MAX), abs_row);
                if cell.spacer {
                    continue;
                }
                line.push(cell.ch);
            }
            let line = line.trim_end();
            out.push_str(line);

            // Newline between physical rows — unless this row soft-wraps onto
            // the next, in which case the logical line continues unbroken.
            let continues = self.abs_wrapped(abs_row);
            if abs_row != end.row && !continues {
                out.push('\n');
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cell;

    fn screen_with(cols: u16, rows: u16, sb_max: usize, lines: &[&str]) -> Screen {
        let mut s = Screen::new(cols, rows, sb_max);
        for (r, text) in lines.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                // Test fixture lines are always short, well under u16::MAX.
                let col = u16::try_from(c).expect("test column fits in u16");
                let row = u16::try_from(r).expect("test row fits in u16");
                s.set_cell(
                    col,
                    row,
                    Cell {
                        ch,
                        ..Cell::default()
                    },
                );
            }
        }
        s
    }

    fn pt(col: u16, row: usize) -> Point {
        Point { col, row }
    }

    #[test]
    fn ordered_normalizes_drag_direction() {
        let sel = Selection {
            start: pt(5, 2),
            end: pt(1, 0),
        };
        let (s, e) = sel.ordered();
        assert_eq!(s, pt(1, 0));
        assert_eq!(e, pt(5, 2));
    }

    #[test]
    fn contains_within_and_outside() {
        let sel = Selection {
            start: pt(1, 0),
            end: pt(2, 1),
        };
        assert!(sel.contains(0, 1));
        assert!(sel.contains(1, 2));
        assert!(!sel.contains(0, 0));
        assert!(!sel.contains(2, 0));
    }

    #[test]
    fn empty_selection_yields_empty_text() {
        let s = screen_with(10, 3, 0, &["hello"]);
        let sel = Selection::new(pt(0, 0));
        assert_eq!(s.selected_text(&sel), "");
    }

    #[test]
    fn single_row_substring() {
        let s = screen_with(10, 3, 0, &["hello world"]);
        let sel = Selection {
            start: pt(0, 0),
            end: pt(4, 0),
        };
        assert_eq!(s.selected_text(&sel), "hello");
    }

    #[test]
    fn multi_row_inserts_newlines_and_trims_padding() {
        let s = screen_with(10, 3, 0, &["abc", "defgh", "x"]);
        let sel = Selection {
            start: pt(0, 0),
            end: pt(9, 2),
        };
        assert_eq!(s.selected_text(&sel), "abc\ndefgh\nx");
    }

    #[test]
    fn soft_wrapped_rows_rejoin_without_newline() {
        let mut s = screen_with(5, 3, 0, &["abcde", "fgh"]);
        s.set_wrapped(0, true); // row 0 continues onto row 1
        let sel = Selection {
            start: pt(0, 0),
            end: pt(4, 1),
        };
        assert_eq!(s.selected_text(&sel), "abcdefgh");
    }

    #[test]
    fn spacer_cells_are_not_double_counted() {
        let mut s = Screen::new(10, 3, 0);
        s.set_cell(
            0,
            0,
            Cell {
                ch: '中',
                wide: true,
                ..Cell::default()
            },
        );
        s.set_cell(
            1,
            0,
            Cell {
                ch: ' ',
                spacer: true,
                ..Cell::default()
            },
        );
        s.set_cell(
            2,
            0,
            Cell {
                ch: 'x',
                ..Cell::default()
            },
        );
        let sel = Selection {
            start: pt(0, 0),
            end: pt(2, 0),
        };
        assert_eq!(s.selected_text(&sel), "中x");
    }
}
