//! Liveness of the Wayland reader thread, as seen from the UI thread.
//!
//! Every window in this workspace is fed by a reader thread that owns the
//! producer half of an SPSC event ring. When the compositor goes away the
//! reader returns and drops that producer, which closes the ring. A loop
//! that *blocks* on `pop_block` learns about this for free -- it gets
//! `None`. But the layer-shell windows (`BarWindow`, `LauncherWindow`)
//! drain with the non-blocking `pop()` and then go do timer-driven work,
//! so for them an empty ring and a dead ring look exactly alike. That is
//! how a compositor restart used to leave orphaned daemons alive forever:
//! nothing in their tick ever asked whether the far end still existed.
//!
//! [`ReaderHealth`] is that question, given an owner and a memory. It
//! *latches*: once the producer is gone it can never come back (the ring is
//! single-use and the reader thread is not restarted), so a window that
//! observes the closure once must keep reporting it even if a later caller
//! asks again. Latching also keeps the check honest under the drain-first
//! ordering below -- see [`ReaderHealth::observe`].

/// Whether the reader thread feeding an event ring is still alive.
///
/// Constructed [`Alive`](Self::Alive) and driven by [`ReaderHealth::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReaderState {
    /// The producer is still held by a running reader thread.
    #[default]
    Alive,
    /// The producer has been dropped and the ring is drained: no further
    /// event will ever arrive, so the window must tear down.
    Stopped,
}

impl ReaderState {
    /// Whether this state means the window should keep running.
    #[must_use]
    pub const fn is_alive(self) -> bool {
        matches!(self, Self::Alive)
    }
}

/// Latching tracker for reader-thread liveness.
///
/// Kept as a field on each window rather than recomputed inline so the
/// "once dead, always dead" rule lives in exactly one place and is unit
/// testable without standing up a compositor.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReaderHealth {
    state: ReaderState,
}

impl ReaderHealth {
    /// A tracker that has not yet seen a closure.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: ReaderState::Alive,
        }
    }

    /// The last observed state, without re-checking the ring.
    #[must_use]
    pub const fn state(&self) -> ReaderState {
        self.state
    }

    /// Whether the reader is still believed to be running.
    #[must_use]
    pub const fn is_alive(&self) -> bool {
        self.state.is_alive()
    }

    /// Fold a fresh observation of the ring into the tracker.
    ///
    /// `closed` is the consumer's `is_closed()` -- the producer has been
    /// dropped -- and `drained` says the ring holds no more buffered
    /// events. Both are required: a reader can exit having already pushed
    /// the events that tell the window *why* (a `wl_display.error`, say),
    /// and dropping those on the floor would turn a diagnosable protocol
    /// fault into a silent exit. So callers drain first, then observe, and
    /// a closed-but-unread ring still reports [`ReaderState::Alive`] for
    /// one more pass.
    ///
    /// Latches: once [`ReaderState::Stopped`] is reached, later calls
    /// cannot revive it regardless of what they pass.
    pub fn observe(&mut self, closed: bool, drained: bool) -> ReaderState {
        if self.state == ReaderState::Stopped || (closed && drained) {
            self.state = ReaderState::Stopped;
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_tracker_believes_the_reader_is_alive() {
        let health = ReaderHealth::new();
        assert!(health.is_alive());
        assert_eq!(health.state(), ReaderState::Alive);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(ReaderHealth::default().state(), ReaderHealth::new().state());
    }

    #[test]
    fn an_open_ring_keeps_the_reader_alive() {
        let mut health = ReaderHealth::new();
        assert_eq!(health.observe(false, true), ReaderState::Alive);
        assert_eq!(health.observe(false, false), ReaderState::Alive);
        assert!(health.is_alive());
    }

    #[test]
    fn a_closed_but_undrained_ring_is_still_alive_so_final_events_are_read() {
        // The reader may have pushed a `wl_display.error` on its way out;
        // reporting Stopped here would discard the explanation.
        let mut health = ReaderHealth::new();
        assert_eq!(health.observe(true, false), ReaderState::Alive);
        assert!(health.is_alive());
    }

    #[test]
    fn a_closed_and_drained_ring_stops_the_window() {
        let mut health = ReaderHealth::new();
        assert_eq!(health.observe(true, true), ReaderState::Stopped);
        assert!(!health.is_alive());
    }

    #[test]
    fn the_stopped_state_latches_and_cannot_be_revived() {
        let mut health = ReaderHealth::new();
        health.observe(true, true);
        // Nothing a later caller passes may resurrect the reader.
        assert_eq!(health.observe(false, false), ReaderState::Stopped);
        assert_eq!(health.observe(false, true), ReaderState::Stopped);
        assert_eq!(health.observe(true, false), ReaderState::Stopped);
        assert!(!health.is_alive());
    }

    #[test]
    fn draining_after_closure_is_the_expected_two_pass_sequence() {
        // Pass 1: reader exited, events still buffered -> keep going.
        // Pass 2: those events consumed -> tear down.
        let mut health = ReaderHealth::new();
        assert_eq!(health.observe(true, false), ReaderState::Alive);
        assert_eq!(health.observe(true, true), ReaderState::Stopped);
    }

    #[test]
    fn reader_state_is_alive_discriminates_the_variants() {
        assert!(ReaderState::Alive.is_alive());
        assert!(!ReaderState::Stopped.is_alive());
        assert_eq!(ReaderState::default(), ReaderState::Alive);
    }
}
