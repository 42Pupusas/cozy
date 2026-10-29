//! Per-row style table.
//!
//! Each screen row owns the distinct styles its cells use; a cell stores a
//! `u16` id into its own row's table. Id 0 is the default style and is never
//! stored. Clearing a row empties its table, so a style never outlives the
//! row that uses it and no table is shared between rows.
//!
//! Rewriting cells in place without clearing the row (a TUI repainting one
//! line) can leave entries no cell uses. When the table reaches its limit it
//! is rebuilt from that row's ids: O(cols), local to the row.

use std::ops::Range;

use crate::Attrs;

/// A cell's render style. Placement (wide lead / spacer) is not part of it:
/// both halves of a wide glyph share one style.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub attrs: Attrs,
    pub link_id: u32,
}

impl Style {
    pub const fn new(attrs: Attrs, link_id: u32) -> Self {
        Self {
            attrs: attrs.without_placement(),
            link_id,
        }
    }
}

#[derive(Default)]
pub struct RowStyles {
    entries: Vec<Style>,
}

impl RowStyles {
    pub fn get(&self, id: u16) -> Style {
        match id {
            0 => Style::default(),
            _ => self.entries[usize::from(id) - 1],
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Become a copy of `other`, reusing this table's allocation.
    pub fn copy_from(&mut self, other: &Self) {
        self.entries.clone_from(&other.entries);
    }

    /// The id of `style` in this row, adding it if new.
    ///
    /// `ids` is the row's id plane. `overwritten` holds the columns the
    /// caller is about to replace; a rebuild treats them as unused, so the
    /// table always has room after one.
    pub fn intern(
        &mut self,
        style: Style,
        ids: &mut [u16],
        overwritten: Range<usize>,
        limit: usize,
    ) -> u16 {
        if style == Style::default() {
            return 0;
        }
        if let Some(index) = self.entries.iter().rposition(|known| *known == style) {
            return Self::id(index);
        }
        if self.entries.len() >= limit {
            ids[overwritten].fill(0);
            self.rebuild(ids);
        }
        self.entries.push(style);
        Self::id(self.entries.len() - 1)
    }

    /// Keep only the entries `ids` still refers to, renumbering `ids`.
    fn rebuild(&mut self, ids: &mut [u16]) {
        let mut renumbered = vec![0u16; self.entries.len() + 1];
        let mut kept = Vec::with_capacity(ids.len().min(self.entries.len()));
        for id in ids.iter_mut().filter(|id| **id != 0) {
            let slot = &mut renumbered[usize::from(*id)];
            if *slot == 0 {
                kept.push(self.entries[usize::from(*id) - 1]);
                *slot = Self::id(kept.len() - 1);
            }
            *id = *slot;
        }
        self.entries = kept;
    }

    fn id(index: usize) -> u16 {
        u16::try_from(index + 1).expect("row style table stays under its u16 limit")
    }
}

#[cfg(test)]
mod tests {
    use super::{RowStyles, Style};
    use crate::{Attrs, Color};

    fn style(n: u8) -> Style {
        Style::new(Attrs::with_colors(Color::Rgb(n, 0, 0), Color::Default), 0)
    }

    #[test]
    fn default_style_is_id_zero_and_never_stored() {
        let mut table = RowStyles::default();
        let mut ids = [0u16; 4];
        assert_eq!(table.intern(Style::default(), &mut ids, 0..1, 8), 0);
        assert!(table.entries.is_empty());
    }

    #[test]
    fn repeated_style_reuses_its_id() {
        let mut table = RowStyles::default();
        let mut ids = [0u16; 4];
        let first = table.intern(style(1), &mut ids, 0..1, 8);
        assert_eq!(table.intern(style(1), &mut ids, 1..2, 8), first);
        assert_eq!(table.entries.len(), 1);
    }

    #[test]
    fn full_table_drops_unused_entries_and_keeps_live_ones() {
        let mut table = RowStyles::default();
        let mut ids = [0u16; 2];
        // Repaint column 0 over and over; column 1 keeps the first style.
        ids[1] = table.intern(style(0), &mut ids, 1..2, 4);
        for n in 1..=20 {
            ids[0] = table.intern(style(n), &mut ids, 0..1, 4);
            assert!(table.entries.len() <= 4);
        }
        assert!(table.get(ids[0]) == style(20));
        assert!(table.get(ids[1]) == style(0));
    }
}
