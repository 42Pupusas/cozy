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

use crate::toolbar::Target;

/// Whatever the pointer is over: a row in one of the panes, or a control.
///
/// One enum rather than two `Option` fields on the window, because at most one
/// thing can be hovered at a time. Two fields would make "a row *and* a button
/// are both lit" representable, and the day a motion event set one without
/// clearing the other, that is exactly what would be painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// An entry in the tile field, or a row in the places list.
    Item(Pane, usize),
    /// A control in the toolbar.
    Control(Target),
    /// The tile that opens the next page of smaller entries.
    Overflow,
}

/// Which pane a hovered row belongs to.
///
/// The two panes number their rows independently, so a bare index is
/// ambiguous: row 2 of the list and row 2 of the places pane are different
/// rows, and a single `Option<usize>` would light both. Pairing the index with
/// its pane makes that mistake unrepresentable rather than merely unlikely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The tile field.
    Tiles,
    /// The places sidebar.
    #[allow(dead_code)]
    Sidebar,
}

/// What the pointer is over, and whether the last update changed it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hover {
    spot: Option<Spot>,
}

impl Hover {
    /// Nothing hovered.
    #[must_use]
    pub const fn none() -> Self {
        Self { spot: None }
    }

    /// The hovered spot, if any.
    ///
    /// The painter asks [`Self::is`] or [`Self::control`] instead, since it
    /// walks rows; this is for a caller that needs the target itself (a context
    /// menu's subject, say).
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn spot(self) -> Option<Spot> {
        self.spot
    }

    /// Whether `row` of `pane` is the hovered one.
    ///
    /// Takes the pane as well as the index so that "nothing hovered" can never
    /// accidentally match row 0 -- the bug a bare `self.row == row` comparison
    /// invites -- and so the two panes cannot light each other's rows.
    #[must_use]
    pub fn is(self, pane: Pane, row: usize) -> bool {
        self.spot == Some(Spot::Item(pane, row))
    }

    /// The hovered toolbar control, if the pointer is over one.
    #[must_use]
    pub const fn control(self) -> Option<Target> {
        match self.spot {
            Some(Spot::Control(target)) => Some(target),
            _ => None,
        }
    }

    /// Whether the pointer is over the overflow tile.
    #[must_use]
    pub fn is_overflow(self) -> bool {
        self.spot == Some(Spot::Overflow)
    }

    /// Move the hover to `spot`, reporting whether a repaint is owed.
    ///
    /// The return value is the whole point: motion events arrive per pixel of
    /// pointer travel, but the highlight only changes when the thing underneath
    /// does, so only that transition earns a frame.
    pub fn move_to(&mut self, spot: Option<Spot>) -> bool {
        let changed = self.spot != spot;
        self.spot = spot;
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
    use super::{Hover, Pane, Spot};
    use crate::toolbar::Target;

    /// Trait rather than a free function, so the tests read as `hover.to_row(..)`
    /// and no plain `fn` sneaks in beside the type it operates on.
    trait HoverRow {
        /// Move the hover to `row` of `pane`, reporting whether a repaint is owed.
        fn to_row(&mut self, pane: Pane, row: usize) -> bool;
        /// Move the hover off the rows entirely.
        fn off_rows(&mut self) -> bool;
    }

    impl HoverRow for Hover {
        fn to_row(&mut self, pane: Pane, row: usize) -> bool {
            self.move_to(Some(Spot::Item(pane, row)))
        }

        fn off_rows(&mut self) -> bool {
            self.move_to(None)
        }
    }

    #[test]
    fn a_fresh_hover_has_no_row() {
        let hover = Hover::none();
        assert_eq!(hover.spot(), None);
        assert_eq!(hover.control(), None);
        assert!(
            !hover.is(Pane::Tiles, 0),
            "nothing hovered must not match row 0"
        );
    }

    /// The `Option` comparison exists precisely so an empty hover cannot be
    /// mistaken for the first row -- an easy off-by-default bug.
    #[test]
    fn an_empty_hover_never_matches_a_row() {
        let hover = Hover::default();
        for row in 0..4 {
            assert!(!hover.is(Pane::Tiles, row));
            assert!(!hover.is(Pane::Sidebar, row));
        }
    }

    #[test]
    fn moving_to_a_new_row_owes_a_repaint() {
        let mut hover = Hover::none();
        assert!(hover.to_row(Pane::Tiles, 3), "first hover is a change");
        assert!(hover.is(Pane::Tiles, 3));
        assert!(hover.to_row(Pane::Tiles, 4), "a different row is a change");
        assert!(!hover.is(Pane::Tiles, 3));
        assert!(hover.is(Pane::Tiles, 4));
    }

    /// The frame-saving property: dragging the pointer within one row must not
    /// ask for a repaint on every motion event.
    #[test]
    fn staying_on_the_same_row_owes_nothing() {
        let mut hover = Hover::none();
        assert!(hover.to_row(Pane::Tiles, 2));
        for _ in 0..64 {
            assert!(
                !hover.to_row(Pane::Tiles, 2),
                "re-hovering the same row must be free"
            );
        }
    }

    /// The same frame-saving property for the toolbar: sliding the pointer
    /// across one button must cost exactly one repaint, not one per pixel.
    #[test]
    fn staying_on_the_same_control_owes_nothing() {
        let mut hover = Hover::none();
        assert!(hover.move_to(Some(Spot::Control(Target::Back))));
        for _ in 0..64 {
            assert!(!hover.move_to(Some(Spot::Control(Target::Back))));
        }
        assert_eq!(hover.control(), Some(Target::Back));
    }

    #[test]
    fn moving_between_controls_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.move_to(Some(Spot::Control(Target::Back)));
        assert!(
            hover.move_to(Some(Spot::Control(Target::Forward))),
            "the neighbouring button must light instead"
        );
        assert_eq!(hover.control(), Some(Target::Forward));
    }

    /// The reason `Spot` is one enum and not two fields: a row and a control
    /// can never be lit at the same time.
    #[test]
    fn a_control_replaces_a_hovered_row_rather_than_joining_it() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 5);
        assert!(hover.move_to(Some(Spot::Control(Target::FilterBox))));
        assert!(
            !hover.is(Pane::Tiles, 5),
            "the row must go dark when a button lights up"
        );
        assert_eq!(hover.control(), Some(Target::FilterBox));
        // And back the other way.
        hover.to_row(Pane::Tiles, 5);
        assert_eq!(
            hover.control(),
            None,
            "the button must go dark when a row lights up"
        );
    }

    /// A row index and a control must not be confused for one another.
    #[test]
    fn a_hovered_row_is_not_reported_as_a_control() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Sidebar, 0);
        assert_eq!(hover.control(), None);
    }

    #[test]
    fn leaving_clears_and_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 1);
        assert!(hover.clear(), "losing the highlight is a visible change");
        assert_eq!(hover.spot(), None);
        assert!(!hover.clear(), "clearing twice owes nothing");
        // A hovered control must be cleared by leaving, too, or a button stays
        // lit while the pointer is in another window.
        hover.move_to(Some(Spot::Control(Target::Back)));
        assert!(hover.clear());
        assert_eq!(hover.control(), None);
    }

    /// Moving off the rows (into the empty space below the list) is a change
    /// just like moving onto them, or the last row keeps a stale highlight.
    #[test]
    fn moving_off_the_rows_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 0);
        assert!(hover.off_rows());
        assert!(!hover.off_rows(), "already off owes nothing");
    }

    /// The reason the pane is part of the key: the two panes number their rows
    /// from zero independently, so hovering row 2 of the sidebar must not also
    /// light row 2 of the list beside it.
    #[test]
    fn the_two_panes_do_not_light_each_others_rows() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Sidebar, 2);
        assert!(hover.is(Pane::Sidebar, 2));
        assert!(
            !hover.is(Pane::Tiles, 2),
            "the list must not light the sidebar's row"
        );
    }

    /// Crossing the seam between the panes is a change: otherwise the row the
    /// pointer just left stays lit in the other pane.
    #[test]
    fn crossing_between_panes_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Sidebar, 1);
        assert!(
            hover.to_row(Pane::Tiles, 1),
            "same index, different pane, is still a change"
        );
        assert!(hover.is(Pane::Tiles, 1));
        assert!(!hover.is(Pane::Sidebar, 1));
    }
}
