//! [`RowKind`] -- which family of highlight colours an entry draws from.

use fsmodel::entry::Entry;
use platform::draw::Color;

use crate::theme::Theme;

/// A two-variant enum rather than a `navigable: bool`, so a call site says
/// which family it means and a third family can be added without every
/// caller changing shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// Anything Enter would descend into -- a directory, or a symlink to one.
    Navigable,
    /// A leaf: a regular file, a device, a broken link.
    Leaf,
}

impl RowKind {
    #[must_use]
    pub const fn of(entry: &Entry) -> Self {
        if entry.is_navigable() {
            Self::Navigable
        } else {
            Self::Leaf
        }
    }

    #[must_use]
    pub const fn selection_fill(self, theme: &Theme) -> Color {
        match self {
            Self::Navigable => theme.selection_dir,
            Self::Leaf => theme.selection_file,
        }
    }
}
