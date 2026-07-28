//! Pointer state: where the cursor is, what is held, and what that means.
//!
//! `wl_pointer` reports physical facts (entered, moved to x/y, button 0x110
//! went down). A GUI wants gestures: "the user clicked this row", "the user
//! is dragging", "the list scrolled three lines". [`PointerState`] is the
//! translation layer, kept apart from the window so the gesture rules are
//! testable without a compositor.
//!
//! Coordinates are **logical** on the wire and physical in the framebuffer,
//! so everything here is stored logical and converted on demand by the
//! window, which owns the scale.

use crate::protocol::{Fixed, PointerButton};
use crate::toplevel::modifiers::ModifierState;

/// One wheel detent, in `wl_fixed` units.
///
/// `wl_pointer.axis` reports a continuous scroll distance, not a click
/// count; a classic mouse wheel notch is 15.0 (`15 * 256` fixed). Dividing
/// by this turns free-scrolling touchpad deltas and discrete wheel clicks
/// into the same unit -- lines -- so callers do not need two code paths.
const DETENT: i32 = 15;

/// Where the pointer is, in logical pixels relative to the surface.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

/// A pointer gesture the window reports to its owner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PointerEventKind {
    /// The pointer entered the surface.
    Enter(Position),
    /// The pointer left. Carries no position: it is outside the surface, and
    /// inventing a last-known coordinate invites hover highlights that never
    /// clear.
    Leave,
    /// The pointer moved to a new position.
    Motion(Position),
    /// A button went down.
    Press {
        button: PointerButton,
        at: Position,
        modifiers: ModifierState,
    },
    /// A button came up.
    Release {
        button: PointerButton,
        at: Position,
        modifiers: ModifierState,
    },
    /// A vertical scroll, in lines. Positive scrolls the content *down*
    /// (the view moves toward the end), matching `wl_pointer.axis` sign.
    Scroll {
        lines: i32,
        at: Position,
        modifiers: ModifierState,
    },
}

/// Tracks pointer position and button state across events.
#[derive(Debug, Default)]
pub struct PointerState {
    /// Last known position; meaningless while `inside` is false.
    position: Position,
    /// Whether the pointer is currently over our surface.
    inside: bool,
    /// The button currently held, if any.
    ///
    /// One button, not a set: a GUI drag is driven by whichever button
    /// started it, and modelling chords would add state no caller reads.
    held: Option<PointerButton>,
    /// Serial of the most recent button press, which the compositor requires
    /// to start an interactive move/resize or open a popup.
    last_serial: u32,
}

impl PointerState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Current position in logical pixels.
    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
    }

    /// Whether the pointer is over the surface.
    #[must_use]
    pub const fn is_inside(&self) -> bool {
        self.inside
    }

    /// The button being held, if any.
    #[must_use]
    pub const fn held(&self) -> Option<PointerButton> {
        self.held
    }

    /// Whether a drag is in progress (any button held).
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.held.is_some()
    }

    /// The last button-press serial, for requests that need one.
    #[must_use]
    pub const fn last_serial(&self) -> u32 {
        self.last_serial
    }

    /// `wl_pointer.enter`.
    pub const fn enter(&mut self, x: Fixed, y: Fixed) -> PointerEventKind {
        self.inside = true;
        self.position = Self::to_position(x, y);
        PointerEventKind::Enter(self.position)
    }

    /// `wl_pointer.leave`.
    ///
    /// Any held button is dropped. The release will be delivered to whoever
    /// the pointer went to, not to us, so keeping the button held would
    /// leave this state stuck mid-drag forever.
    pub const fn leave(&mut self) -> PointerEventKind {
        self.inside = false;
        self.held = None;
        PointerEventKind::Leave
    }

    /// `wl_pointer.motion`.
    pub const fn motion(&mut self, x: Fixed, y: Fixed) -> PointerEventKind {
        self.position = Self::to_position(x, y);
        // A compositor may send motion without a preceding enter when it
        // warps the pointer; trust the motion and treat us as entered rather
        // than dropping the event.
        self.inside = true;
        PointerEventKind::Motion(self.position)
    }

    /// `wl_pointer.button`.
    ///
    /// Returns `None` for buttons we do not model, so an exotic thumb button
    /// is ignored rather than reported as some other button.
    pub fn button(
        &mut self,
        serial: u32,
        code: u32,
        pressed: bool,
        modifiers: ModifierState,
    ) -> Option<PointerEventKind> {
        let button = PointerButton::from_code(code)?;
        if pressed {
            self.held = Some(button);
            self.last_serial = serial;
            Some(PointerEventKind::Press {
                button,
                at: self.position,
                modifiers,
            })
        } else {
            // Only clear the held button if *this* is the one held: a
            // release for a different button must not cancel a live drag.
            if self.held == Some(button) {
                self.held = None;
            }
            Some(PointerEventKind::Release {
                button,
                at: self.position,
                modifiers,
            })
        }
    }

    /// `wl_pointer.axis`, vertical only.
    ///
    /// Returns `None` for horizontal scrolls (axis 1) and for deltas too
    /// small to make a line, so a caller never has to filter zero-line
    /// scroll events out of its own event stream.
    #[must_use]
    pub const fn axis(
        &self,
        axis: u32,
        value: Fixed,
        modifiers: ModifierState,
    ) -> Option<PointerEventKind> {
        // Axis 0 is vertical; horizontal scrolling has no meaning for the
        // vertical lists this window type serves.
        if axis != 0 {
            return None;
        }
        let lines = Self::lines_for(value);
        if lines == 0 {
            return None;
        }
        Some(PointerEventKind::Scroll {
            lines,
            at: self.position,
            modifiers,
        })
    }

    /// Convert a scroll distance to whole lines, preserving sign.
    ///
    /// Rounds away from zero so a partial detent -- every touchpad flick --
    /// still moves the view by one line instead of being silently dropped.
    const fn lines_for(value: Fixed) -> i32 {
        let px = value.to_pixels();
        if px == 0 {
            return 0;
        }
        let magnitude = (px.abs() + DETENT - 1) / DETENT;
        if px < 0 { -magnitude } else { magnitude }
    }

    /// `wl_fixed` pair to logical pixels.
    const fn to_position(x: Fixed, y: Fixed) -> Position {
        Position {
            x: x.to_pixels(),
            y: y.to_pixels(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `wl_fixed` is 24.8, so a whole pixel is 256 units.
    const fn px(n: i32) -> Fixed {
        Fixed(n * 256)
    }

    fn mods() -> ModifierState {
        ModifierState::default()
    }

    #[test]
    fn a_fresh_pointer_is_outside_and_idle() {
        let p = PointerState::new();
        assert!(!p.is_inside());
        assert!(!p.is_dragging());
        assert_eq!(p.held(), None);
        assert_eq!(p.position(), Position { x: 0, y: 0 });
    }

    #[test]
    fn entering_records_the_position() {
        let mut p = PointerState::new();
        let ev = p.enter(px(10), px(20));
        assert_eq!(ev, PointerEventKind::Enter(Position { x: 10, y: 20 }));
        assert!(p.is_inside());
        assert_eq!(p.position(), Position { x: 10, y: 20 });
    }

    #[test]
    fn motion_updates_the_position() {
        let mut p = PointerState::new();
        p.enter(px(0), px(0));
        let ev = p.motion(px(5), px(7));
        assert_eq!(ev, PointerEventKind::Motion(Position { x: 5, y: 7 }));
        assert_eq!(p.position(), Position { x: 5, y: 7 });
    }

    /// Coordinates are fixed-point, not pixels. Reading them raw would place
    /// every click 256x too far down and to the right.
    #[test]
    fn fixed_point_coordinates_are_converted_not_read_raw() {
        let mut p = PointerState::new();
        // 2.5 logical pixels.
        p.enter(Fixed(640), Fixed(640));
        assert_eq!(p.position(), Position { x: 2, y: 2 });
    }

    #[test]
    fn leaving_clears_inside() {
        let mut p = PointerState::new();
        p.enter(px(1), px(1));
        assert_eq!(p.leave(), PointerEventKind::Leave);
        assert!(!p.is_inside());
    }

    /// The release for a drag that left the surface goes to another client,
    /// so leaving has to end the drag or it never ends at all.
    #[test]
    fn leaving_mid_drag_ends_the_drag() {
        let mut p = PointerState::new();
        p.enter(px(1), px(1));
        p.button(1, 0x110, true, mods());
        assert!(p.is_dragging());
        p.leave();
        assert!(!p.is_dragging(), "a drag must not survive the pointer leaving");
        assert_eq!(p.held(), None);
    }

    #[test]
    fn pressing_and_releasing_tracks_the_held_button() {
        let mut p = PointerState::new();
        p.enter(px(3), px(4));

        let ev = p.button(7, 0x110, true, mods());
        assert_eq!(
            ev,
            Some(PointerEventKind::Press {
                button: PointerButton::Left,
                at: Position { x: 3, y: 4 },
                modifiers: mods(),
            })
        );
        assert_eq!(p.held(), Some(PointerButton::Left));
        assert_eq!(p.last_serial(), 7);

        let ev = p.button(8, 0x110, false, mods());
        assert_eq!(
            ev,
            Some(PointerEventKind::Release {
                button: PointerButton::Left,
                at: Position { x: 3, y: 4 },
                modifiers: mods(),
            })
        );
        assert_eq!(p.held(), None);
    }

    /// Right-clicking mid-drag must not cancel the left-button drag.
    #[test]
    fn releasing_another_button_does_not_end_the_drag() {
        let mut p = PointerState::new();
        p.enter(px(0), px(0));
        p.button(1, 0x110, true, mods());
        p.button(2, 0x111, false, mods());
        assert_eq!(
            p.held(),
            Some(PointerButton::Left),
            "a right-button release must not cancel a left-button drag"
        );
    }

    #[test]
    fn an_unmodelled_button_is_ignored_rather_than_misreported() {
        let mut p = PointerState::new();
        assert_eq!(p.button(1, 0x116, true, mods()), None);
        assert_eq!(p.held(), None, "an unknown button must not register as held");
    }

    /// Only presses carry a usable serial; overwriting it on release would
    /// hand the compositor a stale serial for popups and interactive moves.
    #[test]
    fn only_presses_update_the_serial() {
        let mut p = PointerState::new();
        p.button(5, 0x110, true, mods());
        p.button(9, 0x110, false, mods());
        assert_eq!(p.last_serial(), 5);
    }

    #[test]
    fn a_press_carries_the_live_modifiers() {
        let mut p = PointerState::new();
        let ctrl = ModifierState::from_wire(1 << 2, 0, 0, 0);
        let ev = p.button(1, 0x110, true, ctrl);
        let Some(PointerEventKind::Press { modifiers, .. }) = ev else {
            panic!("expected a press");
        };
        assert!(modifiers.ctrl(), "ctrl-click must arrive as a modified press");
    }

    #[test]
    fn one_wheel_detent_is_one_line() {
        let p = PointerState::new();
        let ev = p.axis(0, px(15), mods());
        let Some(PointerEventKind::Scroll { lines, .. }) = ev else {
            panic!("expected a scroll");
        };
        assert_eq!(lines, 1);
    }

    #[test]
    fn scroll_direction_is_preserved() {
        let p = PointerState::new();
        let Some(PointerEventKind::Scroll { lines: down, .. }) = p.axis(0, px(15), mods()) else {
            panic!("expected a scroll");
        };
        let Some(PointerEventKind::Scroll { lines: up, .. }) = p.axis(0, px(-15), mods()) else {
            panic!("expected a scroll");
        };
        assert_eq!(down, 1);
        assert_eq!(up, -1);
    }

    #[test]
    fn several_detents_scroll_several_lines() {
        let p = PointerState::new();
        let Some(PointerEventKind::Scroll { lines, .. }) = p.axis(0, px(45), mods()) else {
            panic!("expected a scroll");
        };
        assert_eq!(lines, 3);
    }

    /// Touchpads emit small continuous deltas. Truncating would make a slow
    /// two-finger drag do nothing at all, which reads as a broken trackpad.
    #[test]
    fn a_partial_detent_still_scrolls_one_line() {
        let p = PointerState::new();
        let Some(PointerEventKind::Scroll { lines, .. }) = p.axis(0, px(3), mods()) else {
            panic!("a partial touchpad delta must still scroll");
        };
        assert_eq!(lines, 1);
    }

    #[test]
    fn a_zero_delta_is_not_reported_as_a_scroll() {
        let p = PointerState::new();
        assert!(p.axis(0, Fixed(0), mods()).is_none());
    }

    #[test]
    fn horizontal_scrolling_is_ignored() {
        let p = PointerState::new();
        assert!(p.axis(1, px(15), mods()).is_none());
    }

    #[test]
    fn a_scroll_reports_the_position_under_the_pointer() {
        let mut p = PointerState::new();
        p.enter(px(40), px(80));
        let Some(PointerEventKind::Scroll { at, .. }) = p.axis(0, px(15), mods()) else {
            panic!("expected a scroll");
        };
        assert_eq!(at, Position { x: 40, y: 80 });
    }

    /// Compositors may warp the pointer, producing motion with no enter.
    /// Dropping those events would freeze the cursor visually.
    #[test]
    fn motion_without_a_preceding_enter_is_still_honoured() {
        let mut p = PointerState::new();
        p.motion(px(9), px(9));
        assert!(p.is_inside());
        assert_eq!(p.position(), Position { x: 9, y: 9 });
    }
}
