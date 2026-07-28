//! The xdg-shell mapping handshake, as an explicit state machine.
//!
//! # Why this deserves its own type
//!
//! xdg-shell's startup is one of the easiest things in Wayland to get subtly
//! wrong, and the failure mode is a protocol error that kills the client:
//!
//! 1. Create the surface and commit **with no buffer attached**.
//! 2. Wait for `xdg_surface.configure`.
//! 3. `ack_configure` with that exact serial.
//! 4. *Now* attach a buffer and commit.
//!
//! Attaching a buffer before the first configure is a protocol violation.
//! `TermWindow` encodes this as a bare three-variant enum plus scattered
//! checks; here the rule is `can_present()`, one method with tests, so the
//! ordering can't be re-broken by an unrelated edit.

/// Where a toplevel is in its lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Created and committed, but the first `xdg_surface.configure` has not
    /// arrived. **No buffer may be attached in this phase.**
    Unconfigured,
    /// Configured and acked at least once; presenting is allowed.
    Mapped,
    /// `xdg_toplevel.close` arrived, or the app asked to quit. We stop
    /// presenting and let the owner tear down.
    Closing,
}

/// Tracks the mapping handshake and the serial that must be acked.
#[derive(Debug, Clone, Copy)]
pub struct Lifecycle {
    phase: Phase,
    /// Serial from the most recent `xdg_surface.configure` that we have not
    /// yet acked. Wayland requires acking the *latest* serial; acking a
    /// stale one leaves the surface unconfigured.
    pending_serial: Option<u32>,
}

impl Lifecycle {
    /// A newly created, not-yet-configured toplevel.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: Phase::Unconfigured,
            pending_serial: None,
        }
    }

    /// The current phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Record an `xdg_surface.configure` serial awaiting acknowledgement.
    ///
    /// A newer configure supersedes an older un-acked one: the compositor
    /// wants the latest serial acked, and during a resize drag several can
    /// queue up before we get a chance to answer.
    pub const fn configure_received(&mut self, serial: u32) {
        self.pending_serial = Some(serial);
    }

    /// Take the serial that needs acking, if any.
    ///
    /// Returning it (rather than acking internally) keeps this type free of
    /// Wayland plumbing: the caller owns the wire, this owns the rule.
    pub const fn take_ack(&mut self) -> Option<u32> {
        self.pending_serial.take()
    }

    /// Note that a configure has been acked, which maps the surface.
    ///
    /// Deliberately ignored while `Closing`: a configure racing in behind a
    /// close request must not resurrect a window we're tearing down.
    pub const fn acked(&mut self) {
        if matches!(self.phase, Phase::Unconfigured) {
            self.phase = Phase::Mapped;
        }
    }

    /// The compositor or user asked the window to close.
    pub const fn close_requested(&mut self) {
        self.phase = Phase::Closing;
    }

    /// Whether a buffer may be attached and committed right now.
    ///
    /// The single rule this type exists to protect.
    #[must_use]
    pub const fn can_present(&self) -> bool {
        matches!(self.phase, Phase::Mapped)
    }

    /// Whether the owner should stop running its loop.
    #[must_use]
    pub const fn is_closing(&self) -> bool {
        matches!(self.phase, Phase::Closing)
    }
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_toplevel_is_unconfigured_and_must_not_present() {
        // Attaching a buffer here is a protocol error that kills the client.
        let l = Lifecycle::new();
        assert_eq!(l.phase(), Phase::Unconfigured);
        assert!(!l.can_present());
        assert!(!l.is_closing());
    }

    #[test]
    fn acking_the_first_configure_maps_the_surface() {
        let mut l = Lifecycle::new();
        l.configure_received(7);
        assert_eq!(l.take_ack(), Some(7));
        l.acked();
        assert_eq!(l.phase(), Phase::Mapped);
        assert!(l.can_present());
    }

    #[test]
    fn a_configure_alone_does_not_map_the_surface() {
        // Mapping requires the *ack*, not merely receiving the configure.
        let mut l = Lifecycle::new();
        l.configure_received(1);
        assert!(!l.can_present());
    }

    #[test]
    fn an_ack_is_consumed_exactly_once() {
        // Re-acking a stale serial is a protocol error.
        let mut l = Lifecycle::new();
        l.configure_received(3);
        assert_eq!(l.take_ack(), Some(3));
        assert_eq!(l.take_ack(), None);
    }

    #[test]
    fn a_newer_configure_supersedes_an_unacked_one() {
        // During a resize drag several configures can queue before we
        // answer; the compositor wants the newest serial acked.
        let mut l = Lifecycle::new();
        l.configure_received(10);
        l.configure_received(11);
        l.configure_received(12);
        assert_eq!(l.take_ack(), Some(12));
        assert_eq!(l.take_ack(), None);
    }

    #[test]
    fn later_configures_keep_the_surface_mapped() {
        let mut l = Lifecycle::new();
        l.configure_received(1);
        l.take_ack();
        l.acked();
        l.configure_received(2);
        l.take_ack();
        l.acked();
        assert_eq!(l.phase(), Phase::Mapped);
        assert!(l.can_present());
    }

    #[test]
    fn closing_stops_presentation() {
        let mut l = Lifecycle::new();
        l.configure_received(1);
        l.take_ack();
        l.acked();
        l.close_requested();
        assert!(l.is_closing());
        assert!(!l.can_present(), "a closing window must not keep painting");
    }

    #[test]
    fn a_configure_racing_behind_a_close_does_not_resurrect_the_window() {
        // The compositor may have queued a configure before it saw our
        // teardown; it must not put us back into Mapped.
        let mut l = Lifecycle::new();
        l.close_requested();
        l.configure_received(9);
        l.take_ack();
        l.acked();
        assert_eq!(l.phase(), Phase::Closing);
        assert!(!l.can_present());
    }

    #[test]
    fn closing_before_ever_being_configured_is_allowed() {
        // A compositor can close a window that never finished mapping.
        let mut l = Lifecycle::new();
        l.close_requested();
        assert!(l.is_closing());
        assert!(!l.can_present());
    }

    #[test]
    fn the_default_is_the_unconfigured_state() {
        assert_eq!(Lifecycle::default().phase(), Phase::Unconfigured);
    }
}
