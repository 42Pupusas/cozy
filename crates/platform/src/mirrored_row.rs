//! One retained on-screen row: its drawn content plus whether it soft-wraps
//! onto the row below.

/// A row of the UI thread's mirror of the grid, as last drawn.
#[derive(Debug, Clone)]
pub struct MirroredRow {
    pub content: grid::DirtyRowContent,
    /// Content length when the row soft-wraps onto the next one; `None` for a
    /// hard line end.
    pub soft_wrap: Option<u16>,
}

impl MirroredRow {
    #[must_use]
    pub const fn blank() -> Self {
        Self {
            content: grid::DirtyRowContent::Cells(Vec::new()),
            soft_wrap: None,
        }
    }

    /// Overwrite this row with a freshly drawn one, reusing its allocation.
    pub fn assign(&mut self, row: &grid::DirtyRow) {
        self.content.clone_from(&row.content);
        self.soft_wrap = row.soft_wrap;
    }

    /// Whether the logical line continues on the next row.
    #[must_use]
    pub const fn continues(&self) -> bool {
        self.soft_wrap.is_some()
    }

    /// Columns that belong to the logical line: the recorded content length
    /// for a soft-wrapped row, the whole row otherwise.
    #[must_use]
    pub fn line_len(&self) -> usize {
        let len = self.content.len();
        self.soft_wrap.map_or(len, |l| usize::from(l).min(len))
    }
}

impl From<&grid::DirtyRow> for MirroredRow {
    fn from(row: &grid::DirtyRow) -> Self {
        Self {
            content: row.content.clone(),
            soft_wrap: row.soft_wrap,
        }
    }
}
