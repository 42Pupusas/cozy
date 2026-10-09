//! [`Caret`] and [`TextRange`] -- positions and spans in a multiline buffer.
//!
//! Split out of [`crate::edit_note`] because "where is the selection" is a
//! different question from "how does typing change the text", and the
//! ordering rules below are worth testing on their own. A selection made by
//! dragging backwards is stored exactly as the user made it -- anchor where
//! they started, caret where they are now -- and only *normalised* when
//! something needs a span. Storing it pre-sorted would lose which end the
//! caret is on, and shift+arrow would grow the wrong side.

/// A caret position: a line index and a **byte** offset into that line,
/// always on a char boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    /// Line index. Ordered before `col`, which is what makes the derived
    /// `Ord` a correct document order.
    pub line: usize,
    /// Byte offset within the line.
    pub col: usize,
}

impl Caret {
    /// A caret at the very start of the buffer.
    pub const START: Self = Self { line: 0, col: 0 };

    /// A caret at `line`/`col`.
    #[must_use]
    pub const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// An inclusive-start, exclusive-end span between two carets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange {
    pub start: Caret,
    pub end: Caret,
}

impl TextRange {
    /// The span between `anchor` and `caret`, in document order.
    ///
    /// Either argument may come first: a selection dragged upward or
    /// leftward is just as valid as one dragged the other way.
    #[must_use]
    pub fn ordered(anchor: Caret, caret: Caret) -> Self {
        if anchor <= caret {
            Self {
                start: anchor,
                end: caret,
            }
        } else {
            Self {
                start: caret,
                end: anchor,
            }
        }
    }

    /// Whether the span covers no characters.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether the span stays within one line.
    #[must_use]
    pub const fn is_single_line(self) -> bool {
        self.start.line == self.end.line
    }

    /// Whether `at` falls inside the span.
    ///
    /// Start-inclusive and end-exclusive, so adjacent spans do not both
    /// claim the same position and a zero-width span contains nothing.
    #[must_use]
    pub fn contains(self, at: Caret) -> bool {
        at >= self.start && at < self.end
    }

    /// The half-open column span this range covers on `line`, given that
    /// line's length, or `None` when the line is outside the range.
    ///
    /// This is what a renderer needs to highlight a selection: for a
    /// multi-line range the first line runs to its end, the last starts at
    /// its beginning, and the lines between are covered entirely.
    #[must_use]
    pub fn columns_on(self, line: usize, line_len: usize) -> Option<(usize, usize)> {
        if line < self.start.line || line > self.end.line {
            return None;
        }
        let from = if line == self.start.line {
            self.start.col
        } else {
            0
        };
        let to = if line == self.end.line {
            self.end.col
        } else {
            // Past the last character, so a selection spanning a line break
            // reads as covering the newline too.
            line_len
        };
        Some((from.min(line_len), to.min(line_len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn carets_order_by_line_before_column() {
        assert!(Caret::new(0, 99) < Caret::new(1, 0));
        assert!(Caret::new(1, 2) < Caret::new(1, 3));
        assert_eq!(Caret::new(2, 5), Caret::new(2, 5));
    }

    #[test]
    fn a_forward_selection_keeps_its_order() {
        let r = TextRange::ordered(Caret::new(0, 1), Caret::new(0, 4));
        assert_eq!(r.start, Caret::new(0, 1));
        assert_eq!(r.end, Caret::new(0, 4));
    }

    #[test]
    fn a_backward_selection_is_normalised() {
        // Dragging right-to-left must yield the same span as left-to-right.
        let back = TextRange::ordered(Caret::new(0, 4), Caret::new(0, 1));
        let forward = TextRange::ordered(Caret::new(0, 1), Caret::new(0, 4));
        assert_eq!(back, forward);
    }

    #[test]
    fn a_selection_spanning_lines_normalises_too() {
        let r = TextRange::ordered(Caret::new(3, 0), Caret::new(1, 2));
        assert_eq!(r.start, Caret::new(1, 2));
        assert_eq!(r.end, Caret::new(3, 0));
    }

    #[test]
    fn an_anchor_equal_to_the_caret_is_empty() {
        let r = TextRange::ordered(Caret::new(1, 1), Caret::new(1, 1));
        assert!(r.is_empty());
        assert!(
            !r.contains(Caret::new(1, 1)),
            "an empty span contains nothing"
        );
    }

    #[test]
    fn containment_is_start_inclusive_and_end_exclusive() {
        let r = TextRange::ordered(Caret::new(0, 2), Caret::new(0, 5));
        assert!(r.contains(Caret::new(0, 2)), "the start is inside");
        assert!(r.contains(Caret::new(0, 4)));
        assert!(!r.contains(Caret::new(0, 5)), "the end is outside");
        assert!(!r.contains(Caret::new(0, 1)));
    }

    #[test]
    fn a_single_line_range_reports_its_own_columns() {
        let r = TextRange::ordered(Caret::new(0, 2), Caret::new(0, 5));
        assert!(r.is_single_line());
        assert_eq!(r.columns_on(0, 10), Some((2, 5)));
        assert_eq!(r.columns_on(1, 10), None, "other lines are untouched");
    }

    #[test]
    fn the_first_line_of_a_multiline_range_runs_to_its_end() {
        let r = TextRange::ordered(Caret::new(0, 3), Caret::new(2, 4));
        assert!(!r.is_single_line());
        assert_eq!(r.columns_on(0, 8), Some((3, 8)));
    }

    #[test]
    fn a_middle_line_is_covered_entirely() {
        let r = TextRange::ordered(Caret::new(0, 3), Caret::new(2, 4));
        assert_eq!(r.columns_on(1, 6), Some((0, 6)));
    }

    #[test]
    fn the_last_line_of_a_multiline_range_stops_at_the_caret() {
        let r = TextRange::ordered(Caret::new(0, 3), Caret::new(2, 4));
        assert_eq!(r.columns_on(2, 9), Some((0, 4)));
    }

    #[test]
    fn columns_never_exceed_the_line_length() {
        // A stale caret past the end of a shortened line must not produce a
        // span that would slice out of bounds.
        let r = TextRange::ordered(Caret::new(0, 2), Caret::new(0, 99));
        let (from, to) = r.columns_on(0, 4).expect("the line is in range");
        assert!(from <= 4 && to <= 4);
    }

    #[test]
    fn a_line_outside_the_range_has_no_columns() {
        let r = TextRange::ordered(Caret::new(1, 0), Caret::new(1, 2));
        assert_eq!(r.columns_on(0, 5), None);
        assert_eq!(r.columns_on(2, 5), None);
    }
}
