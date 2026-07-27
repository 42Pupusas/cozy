//! Decides whether a window's terminal cursor animation should run at all.
//!
//! [`TermWindow`] serves two very different roles through one type:
//!
//! * a **terminal grid** window, which draws a real text cursor and wants it
//!   sliding and blinking; and
//! * a **client-painted** window ([`TermWindow::tick_with_painter`]) -- a
//!   dashboard like `monitor`, a wallpaper, a launcher -- which owns every
//!   pixel itself and has no text cursor whatsoever.
//!
//! The distinction matters far more than it looks, because
//! [`crate::cursor_anim::CursorAnimator`] reports "still animating" *forever*
//! while blink is enabled and the cursor is nominally visible and focused --
//! a fresh animator starts in exactly that state. So for a client-painted
//! window that never calls `set_target`, every tick marked the window dirty,
//! forcing a full present (premultiply + stale-row shm copy +
//! Attach/Damage/Frame/Commit, each a blocking `io_uring` submit) at the tick
//! rate forever, regardless of any redraw gating the client did. That is a
//! pegged CPU core for a window whose pixels never changed.
//!
//! Splitting the rule out here keeps that reasoning in one place and makes it
//! unit-testable: [`TermWindow`] itself can only be built against a live
//! Wayland connection, so a policy left inline there is effectively untestable.
//!
//! [`TermWindow`]: crate::window::TermWindow
//! [`TermWindow::tick_with_painter`]: crate::window::TermWindow::tick_with_painter

/// Tracks a window's paint role and answers the two questions that depend on
/// it: may the cursor animation advance, and should the client keep polling
/// at animation frame rate.
#[derive(Debug, Clone, Copy, Default)]
pub struct CursorAnimGate {
    /// Latched `true` the first time a client painter is supplied.
    ///
    /// Latched rather than recomputed per tick because the role is a property
    /// of the *window*, not of an individual frame: `monitor` paints through
    /// its painter every frame, but deliberately passes a no-op painter on
    /// ticks its redraw gate skipped. Recomputing from "did this tick carry a
    /// painter" would be equally correct there, but would make the role
    /// flicker for any client that mixed `tick()` and `tick_with_painter()`,
    /// and a cursor that animates on alternate ticks is a worse bug than one
    /// that never animates.
    custom_painted: bool,
}

impl CursorAnimGate {
    /// A window that has not yet painted: assumed to be a terminal grid, so
    /// the cursor animates normally until a painter proves otherwise.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            custom_painted: false,
        }
    }

    /// Record that a client painter drove this frame, latching the window
    /// into the client-painted role permanently.
    pub const fn note_custom_paint(&mut self) {
        self.custom_painted = true;
    }

    /// Whether this window is client-painted.
    #[must_use]
    pub const fn is_custom_painted(&self) -> bool {
        self.custom_painted
    }

    /// Whether [`crate::cursor_anim::CursorAnimator::advance`] should be
    /// called this tick. False for client-painted windows, whose cursor is
    /// never drawn and whose animator would therefore only ever generate
    /// spurious dirty marks.
    #[must_use]
    pub const fn may_advance_cursor(&self) -> bool {
        !self.custom_painted
    }

    /// Whether the client should poll at animation frame rate (~60fps)
    /// instead of idling on the full event-wait timeout.
    ///
    /// `cursor_animating` is the animator's own opinion and is ignored for a
    /// client-painted window -- it is permanently true there, and honouring
    /// it would spin the client for an animation that is not running.
    /// `autoscroll_active` applies to both roles: it is driven by pointer
    /// position, not by the cursor.
    #[must_use]
    pub const fn wants_animation_frame(
        &self,
        cursor_animating: bool,
        autoscroll_active: bool,
    ) -> bool {
        if self.custom_painted {
            return autoscroll_active;
        }
        cursor_animating || autoscroll_active
    }
}

#[cfg(test)]
mod tests {
    use super::CursorAnimGate;

    #[test]
    fn a_fresh_window_is_treated_as_a_terminal_grid() {
        let gate = CursorAnimGate::new();
        assert!(!gate.is_custom_painted());
        assert!(
            gate.may_advance_cursor(),
            "a grid window must animate its cursor"
        );
    }

    #[test]
    fn custom_paint_suppresses_the_cursor_animation() {
        // The core regression: a dashboard's animator would otherwise report
        // "animating" forever and mark the window dirty every tick.
        let mut gate = CursorAnimGate::new();
        gate.note_custom_paint();
        assert!(gate.is_custom_painted());
        assert!(
            !gate.may_advance_cursor(),
            "a client-painted window must not advance the cursor animator"
        );
    }

    #[test]
    fn the_custom_painted_role_latches_and_never_reverts() {
        // `monitor` passes a *no-op* painter on gated-out ticks; that must
        // not hand the window back to the grid cursor path.
        let mut gate = CursorAnimGate::new();
        gate.note_custom_paint();
        gate.note_custom_paint();
        assert!(!gate.may_advance_cursor());
    }

    #[test]
    fn a_grid_window_polls_at_frame_rate_while_its_cursor_animates() {
        let gate = CursorAnimGate::new();
        assert!(gate.wants_animation_frame(true, false));
        assert!(
            !gate.wants_animation_frame(false, false),
            "a resting grid cursor must let the loop idle"
        );
    }

    #[test]
    fn a_custom_painted_window_ignores_the_permanently_true_blink() {
        // This is the second half of the bug: even with `advance` suppressed,
        // reporting the animator's opinion here would keep the client spinning
        // at 60fps waiting on an animation that never runs.
        let mut gate = CursorAnimGate::new();
        gate.note_custom_paint();
        assert!(
            !gate.wants_animation_frame(true, false),
            "client-painted windows must ignore cursor blink"
        );
    }

    #[test]
    fn autoscroll_still_forces_frame_rate_polling_in_both_roles() {
        // Autoscroll is driven by pointer position, not the cursor, so it
        // must survive the custom-painted suppression.
        let grid = CursorAnimGate::new();
        assert!(grid.wants_animation_frame(false, true));
        let mut custom = CursorAnimGate::new();
        custom.note_custom_paint();
        assert!(
            custom.wants_animation_frame(false, true),
            "an active autoscroll drag must keep polling even when painted"
        );
    }
}
