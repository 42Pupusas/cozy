//! [`Hover`] -- which list row the pointer is over, if any.
//!
//! Its own type rather than an `Option<usize>` field on the window for one
//! reason: hover is a *repaint trigger*, and the interesting logic is not
//! "what is hovered" but "did that change, and do I owe a frame". A bare field
//! puts that comparison at every call site, and the call sites are exactly
//! where it gets forgotten -- either repainting on every motion event (a
//! wasted frame per pixel of pointer travel, on a surface that redraws the
//! whole window) or not repainting at all and leaving a stale highlight
//! behind.
//!
//! `ListPane` deliberately does not own this: its `motion` handles scrollbar
//! drags, and its docs note that row hover "is the caller's business, since it
//! needs no state this type owns". This is that caller's state.

/// Which pane a hovered row belongs to.
///
/// The two panes number their rows independently, so a bare index is
/// ambiguous: row 2 of the list and row 2 of the places pane are different
/// rows, and a single `Option<usize>` would light both. Pairing the index with
/// its pane makes that mistake unrepresentable rather than merely unlikely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The file list.
    List,
    /// The places sidebar.
    Sidebar,
}

/// The hovered row, and whether the last update changed it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hover {
    row: Option<(Pane, usize)>,
}

impl Hover {
    /// Nothing hovered.
    #[must_use]
    pub const fn none() -> Self {
        Self { row: None }
    }

    /// The hovered pane and row, if the pointer is over one.
    ///
    /// The painter asks [`Self::is`] instead, since it walks rows; this is for
    /// a caller that needs the index itself (a context menu's target, say).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn row(self) -> Option<(Pane, usize)> {
        self.row
    }

    /// Whether `row` of `pane` is the hovered one.
    ///
    /// Takes the pane as well as the index so that "nothing hovered" can never
    /// accidentally match row 0 -- the bug a bare `self.row == row` comparison
    /// invites -- and so the two panes cannot light each other's rows.
    #[must_use]
    pub fn is(self, pane: Pane, row: usize) -> bool {
        self.row == Some((pane, row))
    }

    /// Move the hover to `row` of `pane`, reporting whether a repaint is owed.
    ///
    /// The return value is the whole point: motion events arrive per pixel of
    /// pointer travel, but the highlight only changes when the row underneath
    /// does, so only that transition earns a frame.
    pub fn move_to(&mut self, row: Option<(Pane, usize)>) -> bool {
        let changed = self.row != row;
        self.row = row;
        changed
    }

    /// Clear the hover (the pointer left the surface), reporting whether a
    /// repaint is owed.
    ///
    /// Distinct from `move_to(None)` only in intent, but worth naming: leaving
    /// the window must clear the highlight, and a reader should not have to
    /// work out that `None` means "gone" at the leave-event call site.
    pub fn clear(&mut self) -> bool {
        self.move_to(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{Hover, Pane};

    #[test]
    fn a_fresh_hover_has_no_row() {
        let hover = Hover::none();
        assert_eq!(hover.row(), None);
        assert!(
            !hover.is(Pane::List, 0),
            "nothing hovered must not match row 0"
        );
    }

    /// The `Option` comparison exists precisely so an empty hover cannot be
    /// mistaken for the first row -- an easy off-by-default bug.
    #[test]
    fn an_empty_hover_never_matches_a_row() {
        let hover = Hover::default();
        for row in 0..4 {
            assert!(!hover.is(Pane::List, row));
            assert!(!hover.is(Pane::Sidebar, row));
        }
    }

    #[test]
    fn moving_to_a_new_row_owes_a_repaint() {
        let mut hover = Hover::none();
        assert!(
            hover.move_to(Some((Pane::List, 3))),
            "first hover is a change"
        );
        assert!(hover.is(Pane::List, 3));
        assert!(
            hover.move_to(Some((Pane::List, 4))),
            "a different row is a change"
        );
        assert!(!hover.is(Pane::List, 3));
        assert!(hover.is(Pane::List, 4));
    }

    /// The frame-saving property: dragging the pointer within one row must not
    /// ask for a repaint on every motion event.
    #[test]
    fn staying_on_the_same_row_owes_nothing() {
        let mut hover = Hover::none();
        assert!(hover.move_to(Some((Pane::List, 2))));
        for _ in 0..64 {
            assert!(
                !hover.move_to(Some((Pane::List, 2))),
                "re-hovering the same row must be free"
            );
        }
    }

    #[test]
    fn leaving_clears_and_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.move_to(Some((Pane::List, 1)));
        assert!(hover.clear(), "losing the highlight is a visible change");
        assert_eq!(hover.row(), None);
        assert!(!hover.clear(), "clearing twice owes nothing");
    }

    /// Moving off the rows (into the empty space below the list) is a change
    /// just like moving onto them, or the last row keeps a stale highlight.
    #[test]
    fn moving_off_the_rows_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.move_to(Some((Pane::List, 0)));
        assert!(hover.move_to(None));
        assert!(!hover.move_to(None), "already off owes nothing");
    }

    /// The reason the pane is part of the key: the two panes number their rows
    /// from zero independently, so hovering row 2 of the sidebar must not also
    /// light row 2 of the list beside it.
    #[test]
    fn the_two_panes_do_not_light_each_others_rows() {
        let mut hover = Hover::none();
        hover.move_to(Some((Pane::Sidebar, 2)));
        assert!(hover.is(Pane::Sidebar, 2));
        assert!(
            !hover.is(Pane::List, 2),
            "the list must not light the sidebar's row"
        );
    }

    /// Crossing the seam between the panes is a change: otherwise the row the
    /// pointer just left stays lit in the other pane.
    #[test]
    fn crossing_between_panes_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.move_to(Some((Pane::Sidebar, 1)));
        assert!(
            hover.move_to(Some((Pane::List, 1))),
            "same index, different pane, is still a change"
        );
        assert!(hover.is(Pane::List, 1));
        assert!(!hover.is(Pane::Sidebar, 1));
    }
}
