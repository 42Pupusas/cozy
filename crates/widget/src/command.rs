//! The vocabulary a list pane understands, and the answer it gives back.
//!
//! # Why a semantic command enum rather than key events
//!
//! [`crate::ListPane`] could have taken `platform::toplevel::Key` directly
//! and matched on it. That would have been shorter and wrong: this crate
//! deliberately has **no dependencies**, because that is what lets a 100k-row
//! scroll bug be reproduced in a microsecond-long unit test instead of by
//! launching a compositor and squinting. Taking a platform type would drag
//! Wayland into the dependency graph of every list.
//!
//! Duplicating `Key` here would be worse still -- two enums that must agree
//! forever, with nothing checking that they do.
//!
//! So the layering is: the *binary* owns the keymap (which is a policy
//! question -- does Ctrl-A select all or jump to line start?), translates a
//! key into a [`ListCommand`], and this crate owns what each command *means*
//! for scroll and selection state. The binary's mapper is small enough to
//! test against real platform types; the state machine here is testable
//! against nothing at all.

/// The modifier keys a list actually branches on.
///
/// Only two, because only two change what a click or an arrow means in a
/// list: Ctrl toggles individual rows, Shift extends a range. Alt and Super
/// are window-manager territory and never reach a row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    /// Toggle the clicked row, keeping the rest of the selection.
    pub ctrl: bool,
    /// Extend the selection from the anchor to the clicked row.
    pub shift: bool,
}

impl Mods {
    /// No modifiers held -- a plain click or arrow.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            ctrl: false,
            shift: false,
        }
    }

    /// Only Ctrl held.
    #[must_use]
    pub const fn ctrl() -> Self {
        Self {
            ctrl: true,
            shift: false,
        }
    }

    /// Only Shift held.
    #[must_use]
    pub const fn shift() -> Self {
        Self {
            ctrl: false,
            shift: true,
        }
    }
}

/// Something the user asked the list to do.
///
/// Deliberately about *intent* ("move to the next row"), not mechanism
/// ("Down was pressed"), so the same command can arrive from a key, a menu
/// item, or a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListCommand {
    /// Move the cursor one row, replacing the selection.
    Move(Direction),
    /// Move the cursor one row, extending the selection from the anchor.
    Extend(Direction),
    /// Move the cursor a screenful, replacing the selection.
    Page(Direction),
    /// Jump the cursor to the first or last row.
    Jump(Edge),
    /// Select every row.
    SelectAll,
    /// Drop the selection entirely.
    ClearSelection,
    /// Act on the cursor row -- open a directory, launch a file.
    Activate,
    /// Scroll without moving the cursor, as a wheel does.
    Scroll(i32),
}

/// Which way a cursor movement goes.
///
/// A named direction rather than a bare `i32` delta: callers were otherwise
/// free to pass `-1` or `1` *or* `-3`, and a list that jumps three rows per
/// arrow press is a bug no type would have caught.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    /// The row delta this direction applies.
    #[must_use]
    pub const fn delta(self) -> i32 {
        match self {
            Self::Up => -1,
            Self::Down => 1,
        }
    }
}

/// Which end of the list a jump targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
}

/// What a pane did with an input.
///
/// The distinction between [`Self::Ignored`] and [`Self::Redraw`] is not
/// cosmetic: a window that repaints on every event burns a frame for every
/// pixel of pointer motion over a static list. Reporting "nothing changed"
/// is what lets the caller skip the present entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneResponse {
    /// Nothing changed; no repaint is owed.
    Ignored,
    /// Visible state changed; repaint.
    Redraw,
    /// The user asked to act on this row. Implies a repaint too, since
    /// activating almost always changes what is on screen.
    Activate(usize),
}

impl PaneResponse {
    /// `Redraw` when `changed`, otherwise `Ignored`.
    ///
    /// The widgets already report whether they moved, so this is the seam
    /// that turns those bools into a response without an `if` at each site.
    #[must_use]
    pub const fn redraw_if(changed: bool) -> Self {
        if changed { Self::Redraw } else { Self::Ignored }
    }

    /// Whether the caller owes a repaint.
    #[must_use]
    pub const fn needs_redraw(self) -> bool {
        matches!(self, Self::Redraw | Self::Activate(_))
    }
}

#[cfg(test)]
mod tests {
    use super::{Direction, Mods, PaneResponse};

    #[test]
    fn directions_carry_single_row_deltas() {
        assert_eq!(Direction::Up.delta(), -1);
        assert_eq!(Direction::Down.delta(), 1);
    }

    #[test]
    fn modifier_shorthands_set_exactly_one_flag() {
        assert_eq!(Mods::none(), Mods::default());
        assert!(Mods::ctrl().ctrl && !Mods::ctrl().shift);
        assert!(Mods::shift().shift && !Mods::shift().ctrl);
    }

    #[test]
    fn redraw_if_maps_a_changed_flag_to_a_response() {
        assert_eq!(PaneResponse::redraw_if(true), PaneResponse::Redraw);
        assert_eq!(PaneResponse::redraw_if(false), PaneResponse::Ignored);
    }

    /// An activation changes what is on screen, so it must not be mistaken
    /// for a no-op by a caller that only checks `needs_redraw`.
    #[test]
    fn activation_and_redraw_both_owe_a_repaint_but_ignored_does_not() {
        assert!(PaneResponse::Redraw.needs_redraw());
        assert!(PaneResponse::Activate(3).needs_redraw());
        assert!(!PaneResponse::Ignored.needs_redraw());
    }
}
