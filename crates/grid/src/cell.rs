#![allow(clippy::struct_excessive_bools)]
//! Terminal cell and attribute types.

/// A terminal color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    /// Terminal default (inherits from theme).
    #[default]
    Default,
    /// One of the 256 palette indices (0–255).
    Indexed(u8),
    /// True-color RGB.
    Rgb(u8, u8, u8),
}

/// Cell graphic attributes (SGR state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

/// A single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The Unicode character occupying this cell (space = empty).
    pub ch: char,
    /// Graphic attributes.
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            attrs: Attrs::default(),
        }
    }
}
