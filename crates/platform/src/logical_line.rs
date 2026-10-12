//! A logical line stitched together from consecutive soft-wrapped rows.
//!
//! Text the terminal autowrapped is one line to the program that printed it.
//! URL detection and word selection work on this joined line so a link or
//! token broken across rows is still found whole; flat indices map back to
//! the `(col, row)` cells they were drawn in.

use grid::Cell;

use crate::mirrored_row::MirroredRow;

/// A contiguous run of columns on one viewport row (inclusive on both ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

/// Cells of one logical line, each tagged with its on-screen `(col, row)`.
#[derive(Debug, Default)]
pub struct LogicalLine {
    cells: Vec<Cell>,
    origins: Vec<(u16, u16)>,
}

impl LogicalLine {
    /// The logical line containing viewport `row`, built from the mirror's
    /// rows. `None` when `row` is outside the mirror.
    #[must_use]
    pub fn gather(rows: &[MirroredRow], row: u16) -> Option<Self> {
        let at = usize::from(row);
        if at >= rows.len() {
            return None;
        }
        let mut first = at;
        while first > 0 && rows[first - 1].continues() {
            first -= 1;
        }
        let mut last = at;
        while last + 1 < rows.len() && rows[last].continues() {
            last += 1;
        }
        let mut line = Self::default();
        for (index, mirrored) in rows.iter().enumerate().take(last + 1).skip(first) {
            let cells = mirrored.content.materialize_cells();
            let take = if index < last {
                mirrored.line_len()
            } else {
                cells.len()
            };
            let row = Self::narrow(index);
            for (col, cell) in cells.into_iter().take(take).enumerate() {
                line.cells.push(cell);
                line.origins.push((Self::narrow(col), row));
            }
        }
        Some(line)
    }

    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Flat index of the cell drawn at `(col, row)`, if it belongs to the line.
    #[must_use]
    pub fn index_of(&self, col: u16, row: u16) -> Option<usize> {
        self.origins.iter().position(|&origin| origin == (col, row))
    }

    /// The `(col, row)` a flat index was drawn at.
    #[must_use]
    pub fn origin(&self, index: usize) -> Option<(u16, u16)> {
        self.origins.get(index).copied()
    }

    /// Per-row spans covering flat indices `start..=end`.
    #[must_use]
    pub fn segments(&self, start: usize, end: usize) -> Vec<RowSpan> {
        let mut spans: Vec<RowSpan> = Vec::new();
        let end = end.min(self.origins.len().saturating_sub(1));
        for &(col, row) in self.origins.iter().take(end + 1).skip(start) {
            match spans.last_mut() {
                Some(span) if span.row == row => span.end_col = col,
                _ => spans.push(RowSpan {
                    row,
                    start_col: col,
                    end_col: col,
                }),
            }
        }
        spans
    }

    fn narrow(value: usize) -> u16 {
        u16::try_from(value).unwrap_or(u16::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, soft_wrap: Option<u16>) -> MirroredRow {
        MirroredRow {
            content: grid::DirtyRowContent::Cells(
                text.chars()
                    .map(|ch| Cell::new(ch, grid::Attrs::default(), 0))
                    .collect(),
            ),
            soft_wrap,
        }
    }

    fn text(line: &LogicalLine) -> String {
        line.cells().iter().map(|c| c.ch).collect()
    }

    #[test]
    fn joins_soft_wrapped_rows_from_any_member_row() {
        let rows = [
            row("$ ls  ", None),
            row("abcdef", Some(6)),
            row("ghijkl", Some(6)),
            row("mn    ", None),
            row("next  ", None),
        ];
        for r in 1..=3 {
            let line = LogicalLine::gather(&rows, r).expect("row is mirrored");
            assert_eq!(text(&line), "abcdefghijklmn    ");
        }
        assert_eq!(text(&LogicalLine::gather(&rows, 4).unwrap()), "next  ");
    }

    #[test]
    fn ragged_reflowed_row_drops_its_padding() {
        let rows = [row("hello ", Some(4)), row("world ", None)];
        let line = LogicalLine::gather(&rows, 0).unwrap();
        assert_eq!(text(&line), "hellworld ");
        assert_eq!(line.index_of(0, 1), Some(4));
        assert_eq!(line.index_of(5, 0), None);
    }

    #[test]
    fn segments_split_per_row() {
        let rows = [row("abcd", Some(4)), row("efgh", None)];
        let line = LogicalLine::gather(&rows, 1).unwrap();
        assert_eq!(
            line.segments(2, 5),
            vec![
                RowSpan {
                    row: 0,
                    start_col: 2,
                    end_col: 3
                },
                RowSpan {
                    row: 1,
                    start_col: 0,
                    end_col: 1
                },
            ]
        );
        assert_eq!(line.origin(5), Some((1, 1)));
    }

    #[test]
    fn row_outside_the_mirror_has_no_line() {
        assert!(LogicalLine::gather(&[row("x", None)], 3).is_none());
    }
}
