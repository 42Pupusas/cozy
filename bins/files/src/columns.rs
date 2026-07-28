//! [`Columns`] -- the file list's four-column geometry.
//!
//! The design guide specifies name/modified/kind/size at 53/20/17/10 percent,
//! with the name column deliberately taking the most width because a filename
//! is the thing being scanned and the metadata is context. Below 780px the
//! guide drops the `modified` column so filenames keep their priority.
//!
//! This is a separate module from the painter because column arithmetic is
//! pure geometry with real edge cases -- percentages that must not overflow
//! their container, a narrow window that must not produce negative widths,
//! a responsive rule that changes which columns exist at all -- and none of
//! that is testable while interleaved with `fill_rect` calls.
//!
//! Sizes and the ellipsis budget are *not* decided here: those need font
//! measurement, which belongs to the painter's tiers.

/// Which metadata column a cell belongs to.
///
/// An enum rather than bare indices so the painter cannot silently draw the
/// size into the kind column after a layout change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Modified,
    Kind,
    Size,
}

impl Column {
    /// The guide's column headings, in the capitalisation they are *drawn*
    /// with.
    ///
    /// The guide reaches this look via `text-transform: uppercase` on 9px
    /// letter-spaced type. We have no letter-spacing control in the text
    /// renderer, and all-caps at our body size reads as shouting rather than
    /// as the quiet structural label the guide intends -- so the heading is
    /// title-cased instead, which is the same signal (these are labels, not
    /// data) at a size we can actually draw.
    ///
    /// Stored pre-capitalised rather than transformed at paint time: a
    /// transform would have to decide what to do with a word like "kind" every
    /// frame, and the answer never changes.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Modified",
            Self::Kind => "Kind",
            Self::Size => "Size",
        }
    }

    /// Whether this column's text hangs off the right edge of its cell.
    ///
    /// Only `size` is right-aligned: numbers compare by magnitude when their
    /// digits line up, and a ragged size column defeats the whole reason for
    /// having one.
    #[must_use]
    pub const fn right_aligned(self) -> bool {
        matches!(self, Self::Size)
    }
}

/// One column's horizontal extent in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub column: Column,
    /// Left edge, absolute within the window.
    pub x: i32,
    pub width: i32,
}

impl Cell {
    /// The x coordinate at which text in this cell should start, given the
    /// pixel `text_width` of the string being drawn.
    ///
    /// Right-aligned columns hang their text off the cell's right edge; every
    /// other column starts at its left. Clamped so an over-wide string starts
    /// at the cell's left rather than being pushed off to the left of it.
    #[must_use]
    pub const fn text_x(self, text_width: i32) -> i32 {
        if self.column.right_aligned() {
            let right = self.x + self.width - text_width;
            if right > self.x { right } else { self.x }
        } else {
            self.x
        }
    }
}

/// The laid-out columns for a list of a given width.
#[derive(Debug, Clone)]
pub struct Columns {
    cells: Vec<Cell>,
}

impl Columns {
    /// Guide percentages, in column order. They sum to 100.
    const SHARES: [(Column, i32); 4] = [
        (Column::Name, 53),
        (Column::Modified, 20),
        (Column::Kind, 17),
        (Column::Size, 10),
    ];

    /// Lay out the columns across `width` pixels starting at `x`, with `gap`
    /// between them and the guide's responsive rule applied.
    ///
    /// `compact` drops the `modified` column, per the guide's sub-780px rule.
    /// Its share is redistributed to `name` rather than left as a hole,
    /// because the point of dropping it is to give filenames the room.
    #[must_use]
    pub fn new(x: i32, width: i32, gap: i32, compact: bool) -> Self {
        let gap = gap.max(0);
        // A container narrower than its own gaps has no room for content at
        // all; return no cells so the painter draws nothing rather than
        // computing negative widths.
        let visible: Vec<(Column, i32)> = if compact {
            Self::SHARES
                .iter()
                .filter(|(c, _)| *c != Column::Modified)
                .copied()
                .collect()
        } else {
            Self::SHARES.to_vec()
        };
        let total_share: i32 = visible.iter().map(|(_, s)| *s).sum();
        let gaps = gap * i32::try_from(visible.len().saturating_sub(1)).unwrap_or(0);
        let content = width - gaps;
        if content <= 0 || total_share <= 0 {
            return Self { cells: Vec::new() };
        }

        let mut cells = Vec::with_capacity(visible.len());
        let mut cursor = x;
        let mut consumed = 0;
        let last = visible.len() - 1;
        for (i, (column, share)) in visible.iter().enumerate() {
            // The final column takes whatever integer division left over,
            // so the columns always exactly fill the container instead of
            // leaving a 1-3px seam that shifts as the window is dragged.
            let w = if i == last {
                content - consumed
            } else {
                content * share / total_share
            };
            let w = w.max(0);
            cells.push(Cell {
                column: *column,
                x: cursor,
                width: w,
            });
            consumed += w;
            cursor += w + gap;
        }
        Self { cells }
    }

    /// Every laid-out cell, in display order.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The cell for one column, if that column is shown at this width.
    ///
    /// The painter iterates [`Self::cells`] rather than asking per column, so
    /// this is the lookup the layout tests assert through -- "the name column
    /// is the widest" is not a statement about iteration order.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cell(&self, column: Column) -> Option<Cell> {
        self.cells.iter().copied().find(|c| c.column == column)
    }

    /// Whether a column survived the responsive rule.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn shows(&self, column: Column) -> bool {
        self.cell(column).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::{Column, Columns};

    /// The columns must exactly fill their container: any shortfall shows as
    /// a seam at the right edge that moves while the window is dragged.
    #[test]
    fn the_columns_exactly_fill_the_container() {
        for width in [200, 640, 641, 999, 1280, 1281] {
            let cols = Columns::new(0, width, 0, false);
            let last = cols.cells().last().copied().expect("cells");
            assert_eq!(
                last.x + last.width,
                width,
                "columns must reach the right edge at width {width}"
            );
        }
    }

    /// Gaps must be accounted for, not overflow the container.
    #[test]
    fn gaps_come_out_of_the_content_not_the_container() {
        let width = 1000;
        let gap = 12;
        let cols = Columns::new(0, width, gap, false);
        let last = cols.cells().last().copied().expect("cells");
        assert_eq!(last.x + last.width, width);
        // Three gaps between four columns.
        let content: i32 = cols.cells().iter().map(|c| c.width).sum();
        assert_eq!(content, width - gap * 3);
    }

    /// The name column must dominate -- it is the thing being read.
    #[test]
    fn the_name_column_gets_the_most_room() {
        let cols = Columns::new(0, 1000, 8, false);
        let name = cols.cell(Column::Name).expect("name").width;
        for other in [Column::Modified, Column::Kind, Column::Size] {
            let w = cols.cell(other).expect("column").width;
            assert!(name > w, "name must exceed {other:?}");
        }
    }

    /// Columns must be laid out left to right without overlapping, or text
    /// from one cell paints over another.
    #[test]
    fn cells_are_ordered_and_never_overlap() {
        let cols = Columns::new(50, 900, 10, false);
        let mut previous_right = 50;
        for cell in cols.cells() {
            assert!(
                cell.x >= previous_right,
                "{:?} starts before the previous cell ended",
                cell.column
            );
            previous_right = cell.x + cell.width;
        }
    }

    /// The layout must honour its starting offset, so the list sits beside
    /// the sidebar rather than under it.
    #[test]
    fn the_layout_starts_where_it_is_told() {
        let cols = Columns::new(210, 800, 8, false);
        let first = cols.cells().first().copied().expect("cells");
        assert_eq!(first.x, 210);
        let last = cols.cells().last().copied().expect("cells");
        assert_eq!(last.x + last.width, 210 + 800);
    }

    /// The guide's responsive rule: narrow windows drop `modified` and give
    /// the room to filenames.
    #[test]
    fn compact_drops_modified_and_widens_the_name() {
        let wide = Columns::new(0, 700, 8, false);
        let narrow = Columns::new(0, 700, 8, true);
        assert!(wide.shows(Column::Modified));
        assert!(!narrow.shows(Column::Modified));
        assert!(
            narrow.cell(Column::Name).expect("name").width
                > wide.cell(Column::Name).expect("name").width,
            "the freed room must go to the filename"
        );
        // The remaining columns still fill the container exactly.
        let last = narrow.cells().last().copied().expect("cells");
        assert_eq!(last.x + last.width, 700);
    }

    /// Only the size column is right-aligned; digits must line up.
    #[test]
    fn only_the_size_column_is_right_aligned() {
        assert!(Column::Size.right_aligned());
        for c in [Column::Name, Column::Modified, Column::Kind] {
            assert!(!c.right_aligned(), "{c:?} must be left-aligned");
        }
    }

    /// A right-aligned cell hangs its text off the right edge.
    #[test]
    fn right_aligned_text_hugs_the_right_edge() {
        let cols = Columns::new(0, 1000, 0, false);
        let size = cols.cell(Column::Size).expect("size");
        assert_eq!(size.text_x(30), size.x + size.width - 30);
    }

    /// A string wider than its cell must start at the cell's left rather
    /// than being pushed off the left of it, which would paint over the
    /// neighbouring column.
    #[test]
    fn oversized_text_does_not_escape_its_cell_to_the_left() {
        let cols = Columns::new(100, 1000, 0, false);
        let size = cols.cell(Column::Size).expect("size");
        assert_eq!(size.text_x(size.width + 500), size.x);
    }

    /// Left-aligned cells ignore the text width entirely.
    #[test]
    fn left_aligned_text_starts_at_the_cell_edge() {
        let cols = Columns::new(0, 1000, 0, false);
        let name = cols.cell(Column::Name).expect("name");
        assert_eq!(name.text_x(0), name.x);
        assert_eq!(name.text_x(9999), name.x);
    }

    /// A container with no usable room yields no cells rather than negative
    /// widths that would make every downstream calculation nonsense.
    #[test]
    fn a_degenerate_width_yields_no_cells() {
        for width in [0, -1, -1000] {
            assert!(Columns::new(0, width, 8, false).cells().is_empty());
        }
        // Width entirely consumed by gaps is equally unusable.
        assert!(Columns::new(0, 20, 30, false).cells().is_empty());
    }

    /// No cell may have a negative width at any size -- a negative width
    /// passed to a truncation budget would wrap around.
    #[test]
    fn no_cell_is_ever_negative() {
        for width in 1..300 {
            for compact in [false, true] {
                for cell in Columns::new(0, width, 4, compact).cells() {
                    assert!(
                        cell.width >= 0,
                        "{:?} negative at width {width}",
                        cell.column
                    );
                }
            }
        }
    }

    /// Every column has a heading, and they are drawn title-cased -- the
    /// readable stand-in for the guide's letter-spaced small caps.
    #[test]
    fn every_column_names_itself() {
        assert_eq!(Column::Name.heading(), "Name");
        assert_eq!(Column::Modified.heading(), "Modified");
        assert_eq!(Column::Kind.heading(), "Kind");
        assert_eq!(Column::Size.heading(), "Size");
        // Every heading is capitalised and none slipped back to lowercase.
        for column in [Column::Name, Column::Modified, Column::Kind, Column::Size] {
            let heading = column.heading();
            let first = heading.chars().next().expect("a heading is never empty");
            assert!(first.is_uppercase(), "{heading} should lead with a capital");
        }
    }
}
