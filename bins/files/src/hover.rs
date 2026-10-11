//! [`Hover`] -- what the pointer is over, if anything.
//!
//! Its own type rather than an `Option` field on the window for one reason:
//! hover is a *repaint trigger*, and the interesting logic is not "what is
//! hovered" but "did that change, and do I owe a frame". A bare field puts
//! that comparison at every call site, and the call sites are exactly where it
//! gets forgotten -- either repainting on every motion event (a wasted frame
//! per pixel of pointer travel) or not repainting at all and leaving a stale
//! state behind.

use tiles::tile_action::TileAction;
use crate::toolbar::Target;

/// Whatever the pointer is over: a tile, a button on one, or a control.
///
/// One enum rather than several `Option` fields on the window, because at most
/// one thing can be hovered at a time. Separate fields would make "a tile
/// *and* a control are both lit" representable, and the day a motion event set
/// one without clearing the other, that is exactly what would be painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spot {
    /// An entry in the tile field, or a row in the places list.
    Item(Pane, usize),
    /// A control in the toolbar.
    Control(Target),
    More,
    /// An action button on the tile of the entry at this row. The tile itself
    /// is hovered too.
    Action(usize, TileAction),
}

/// Which pane a hovered row belongs to.
///
/// The two panes number their rows independently, so a bare index is
/// ambiguous: row 2 of the tiles and row 2 of the places pane are different
/// rows. Pairing the index with its pane makes that mistake unrepresentable
/// rather than merely unlikely.
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
    #[must_use]
    pub const fn spot(self) -> Option<Spot> {
        self.spot
    }

    /// The hovered toolbar control, if the pointer is over one.
    #[must_use]
    pub const fn control(self) -> Option<Target> {
        match self.spot {
            Some(Spot::Control(target)) => Some(target),
            _ => None,
        }
    }

    /// Move the hover to `spot`, reporting whether a repaint is owed.
    ///
    /// The return value is the whole point: motion events arrive per pixel of
    /// pointer travel, but what is lit only changes when the thing underneath
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
    use tiles::tile_action::TileAction;
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
    fn a_fresh_hover_is_over_nothing() {
        let hover = Hover::none();
        assert_eq!(hover.spot(), None);
        assert_eq!(hover.control(), None);
        assert_eq!(Hover::default(), hover);
    }

    #[test]
    fn moving_to_a_new_row_owes_a_repaint() {
        let mut hover = Hover::none();
        assert!(hover.to_row(Pane::Tiles, 3), "first hover is a change");
        assert_eq!(hover.spot(), Some(Spot::Item(Pane::Tiles, 3)));
        assert!(hover.to_row(Pane::Tiles, 4), "a different row is a change");
        assert_eq!(hover.spot(), Some(Spot::Item(Pane::Tiles, 4)));
    }

    /// The frame-saving property: moving within one row must not ask for a
    /// repaint on every motion event.
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

    /// The reason `Spot` is one enum and not several fields: a tile and a
    /// control can never be lit at the same time.
    #[test]
    fn a_control_replaces_a_hovered_row_rather_than_joining_it() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 5);
        assert!(hover.move_to(Some(Spot::Control(Target::FilterBox))));
        assert_eq!(hover.spot(), Some(Spot::Control(Target::FilterBox)));
        hover.to_row(Pane::Tiles, 5);
        assert_eq!(hover.control(), None);
    }

    #[test]
    fn a_hovered_row_is_not_reported_as_a_control() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Sidebar, 0);
        assert_eq!(hover.control(), None);
    }

    #[test]
    fn sliding_across_a_tiles_buttons_changes_the_spot_each_time() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 3);
        assert!(hover.move_to(Some(Spot::Action(3, TileAction::Cut))));
        assert!(
            !hover.move_to(Some(Spot::Action(3, TileAction::Cut))),
            "staying on a button is free"
        );
        assert!(
            hover.move_to(Some(Spot::Action(3, TileAction::Copy))),
            "sliding to the next button is a change"
        );
        assert!(hover.move_to(Some(Spot::Item(Pane::Tiles, 3))));
        assert_eq!(hover.control(), None);
    }

    #[test]
    fn leaving_clears_and_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 1);
        assert!(hover.clear(), "losing the highlight is a visible change");
        assert_eq!(hover.spot(), None);
        assert!(!hover.clear(), "clearing twice owes nothing");
        hover.move_to(Some(Spot::Control(Target::Back)));
        assert!(hover.clear());
        assert_eq!(hover.control(), None);
    }

    /// Moving off the rows is a change just like moving onto them, or the last
    /// row keeps a stale highlight.
    #[test]
    fn moving_off_the_rows_owes_a_repaint() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Tiles, 0);
        assert!(hover.off_rows());
        assert!(!hover.off_rows(), "already off owes nothing");
    }

    /// The reason the pane is part of the key: the two panes number their rows
    /// from zero independently, so hovering row 2 of one must not read as row 2
    /// of the other.
    #[test]
    fn the_two_panes_are_different_spots() {
        let mut hover = Hover::none();
        hover.to_row(Pane::Sidebar, 2);
        assert_ne!(hover.spot(), Some(Spot::Item(Pane::Tiles, 2)));
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
    }
}
