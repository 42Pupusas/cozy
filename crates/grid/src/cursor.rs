//! Cursor state.

/// Visual cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Current cursor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Column (0-indexed).
    pub col: u16,
    /// Row (0-indexed).
    pub row: u16,
    /// Whether the cursor is visible.
    pub visible: bool,
    /// Visual shape.
    pub shape: CursorShape,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            col: 0,
            row: 0,
            visible: true,
            shape: CursorShape::Block,
        }
    }
}
