//! The `zwlr_screencopy_frame_v1` negotiation, as a pure state machine.
//!
//! Between `capture_output` and the pixels landing in our shm there is a
//! small protocol conversation, and every subtle way a screenshot tool breaks
//! lives in it rather than in the socket code:
//!
//! * **`buffer_done` only exists in v3.** A v1/v2 compositor sends `buffer`
//!   and then waits for us. A client that blocks until `buffer_done` hangs
//!   forever against those compositors. The spec resolves this by version:
//!   "for objects version 2 or lower, `wl_shm` buffers are always supported,
//!   ie. the buffer event is guaranteed to be sent", so below v3 the single
//!   `buffer` event *is* the whole menu and licenses `copy` immediately.
//!   This keys off the bound version rather than a round-trip: capture takes
//!   the *next* frame, so `buffer` events arrive whenever the compositor next
//!   composites, which can be long after any `wl_display.sync` `done`.
//! * **`flags` is optional and unordered.** It arrives before `ready`, but a
//!   compositor with nothing to report may skip it, so its absence means "no
//!   flags", never "keep waiting".
//! * **The compositor may offer several buffer types.** v3 sends `buffer`
//!   *and* `linux_dmabuf` and expects us to pick; we only do shm, so dmabuf
//!   offers are recorded-and-ignored rather than mistaken for our parameters.
//!
//! Keeping this separate from the socket means the awkward cases -- v1
//! compositor, missing `flags`, `failed` mid-handshake -- are ordinary unit
//! tests instead of things you can only hit by running on the right machine.

use crate::frame_params::{FrameParams, ParamsError};

/// What the caller should do after feeding in one frame event.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Keep reading events; the handshake isn't decided yet.
    Wait,
    /// The parameter menu is closed. Allocate a buffer to these parameters
    /// and send `copy` -- exactly once.
    SendCopy(FrameParams),
    /// Pixels are in the buffer; `flags` tells you whether they're bottom-up.
    Done { flags: u32 },
    /// The compositor gave up, or offered nothing we can use.
    Failed(HandshakeError),
}

/// Which half of the conversation we're in.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Collecting the buffer-parameter menu, before `copy` is sent.
    Offering,
    /// `copy` has been sent; awaiting `ready` or `failed`.
    Copying,
    /// Terminal: `ready` or `failed` seen, nothing more will be accepted.
    Settled,
}

/// Tracks one frame's negotiation from creation to `ready`/`failed`.
///
/// Deliberately owns no object ids and no connection: the caller matches wire
/// events to its frame object and routes the arguments here, which is what
/// makes every branch below testable without a compositor.
#[derive(Debug)]
pub struct FrameHandshake {
    /// The bound `zwlr_screencopy_manager_v1` version, which decides how the
    /// parameter menu ends -- see the module doc.
    version: u32,
    phase: Phase,
    /// The shm parameters from the `buffer` event, once one has been offered
    /// and validated. `None` after a rejected offer -- see `params_error`.
    offered: Option<FrameParams>,
    /// Why an offer was rejected, kept so that a compositor which offers only
    /// formats we can't decode fails with *that* reason rather than a vague
    /// "no usable buffer".
    params_error: Option<ParamsError>,
    /// The `flags` bitmask, defaulting to zero because the event is optional.
    flags: u32,
}

/// The first interface version that sends `buffer_done`.
const BUFFER_DONE_SINCE: u32 = 3;

impl FrameHandshake {
    /// Begin tracking a freshly created frame object, negotiated at `version`
    /// (the bound `zwlr_screencopy_manager_v1` version).
    #[must_use]
    pub const fn new(version: u32) -> Self {
        Self {
            version,
            phase: Phase::Offering,
            offered: None,
            params_error: None,
            flags: 0,
        }
    }

    /// Record a `buffer` event's parameters.
    ///
    /// Invalid offers are remembered rather than returned: a compositor may
    /// legitimately advertise several buffer types, so one unusable offer is
    /// only fatal if nothing better arrives before the menu closes.
    pub fn on_buffer(&mut self, format: u32, width: u32, height: u32, stride: u32) -> Step {
        if self.phase != Phase::Offering {
            // A late `buffer` after `copy` would mean re-allocating a buffer
            // the compositor is already writing into.
            return Step::Wait;
        }
        match FrameParams::new(format, width, height, stride) {
            // First usable offer wins; later ones are redundant for shm.
            Ok(params) => {
                if self.offered.is_none() {
                    self.offered = Some(params);
                }
                // Below v3 there is no `buffer_done` coming, and the spec
                // guarantees the shm `buffer` event is sent, so this offer is
                // already the complete menu.
                if self.version < BUFFER_DONE_SINCE {
                    return self.close_menu();
                }
            }
            Err(e) => self.params_error = self.params_error.or(Some(e)),
        }
        Step::Wait
    }

    /// Record `buffer_done` (v3+): the menu is closed, so decide now.
    pub fn on_buffer_done(&mut self) -> Step {
        if self.phase != Phase::Offering {
            return Step::Wait;
        }
        self.close_menu()
    }

    /// Record the optional `flags` event.
    pub const fn on_flags(&mut self, flags: u32) -> Step {
        self.flags = flags;
        Step::Wait
    }

    /// Record `ready`: the copy landed.
    pub fn on_ready(&mut self) -> Step {
        if self.phase == Phase::Settled {
            return Step::Wait;
        }
        self.phase = Phase::Settled;
        Step::Done { flags: self.flags }
    }

    /// Record `failed`: the compositor abandoned the capture.
    pub fn on_failed(&mut self) -> Step {
        if self.phase == Phase::Settled {
            return Step::Wait;
        }
        self.phase = Phase::Settled;
        Step::Failed(HandshakeError::CompositorFailed)
    }

    /// Whether `copy` has been sent and we're awaiting the result.
    #[must_use]
    pub const fn is_copying(&self) -> bool {
        matches!(self.phase, Phase::Copying)
    }

    /// Whether the handshake reached `ready` or `failed`.
    #[must_use]
    pub const fn is_settled(&self) -> bool {
        matches!(self.phase, Phase::Settled)
    }

    /// The parameters `copy` was sent against, once the menu has closed.
    ///
    /// The caller needs these again at `ready` time to read the buffer back,
    /// and taking them from here rather than stashing a copy at `SendCopy`
    /// means there is only one record of what was committed to.
    #[must_use]
    pub const fn committed(&self) -> Option<FrameParams> {
        self.offered
    }

    /// Commit to the best offer collected so far, or fail with the most
    /// specific reason available.
    fn close_menu(&mut self) -> Step {
        if let Some(params) = self.offered {
            self.phase = Phase::Copying;
            return Step::SendCopy(params);
        }
        self.phase = Phase::Settled;
        // Prefer the concrete rejection ("format 9999 unsupported") over the
        // generic one; a compositor offering only dmabuf hits the latter.
        Step::Failed(
            self.params_error
                .map_or(HandshakeError::NoUsableBuffer, HandshakeError::Unusable),
        )
    }
}

/// Why a frame negotiation ended without pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HandshakeError {
    /// The compositor sent `failed`: the output vanished, or capture is not
    /// permitted.
    CompositorFailed,
    /// The menu closed with no shm `buffer` offer at all (a dmabuf-only
    /// compositor, which this shm-based client cannot serve).
    NoUsableBuffer,
    /// Every shm offer was one we can't interpret.
    Unusable(ParamsError),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CompositorFailed => {
                write!(f, "compositor reported the capture failed")
            }
            Self::NoUsableBuffer => {
                write!(
                    f,
                    "compositor offered no shared-memory buffer to capture into"
                )
            }
            Self::Unusable(e) => write!(f, "compositor's buffer offer is unusable: {e}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

#[cfg(test)]
mod tests {
    use super::{FrameHandshake, HandshakeError, Step};
    use crate::frame_params::ParamsError;
    use platform::protocol::Format;

    /// The version this crate binds; `buffer_done` exists here.
    const V3: u32 = 3;

    /// A valid 4x3 offer, the shape most of these tests start from.
    fn offer(h: &mut FrameHandshake) -> Step {
        h.on_buffer(Format::Xrgb8888.code(), 4, 3, 16)
    }

    #[test]
    fn v3_flow_closes_the_menu_on_buffer_done() {
        let mut h = FrameHandshake::new(V3);
        // On v3 a lone `buffer` is not yet the whole menu -- dmabuf offers
        // may still follow -- so it must not license `copy` on its own.
        assert_eq!(offer(&mut h), Step::Wait);
        let Step::SendCopy(params) = h.on_buffer_done() else {
            panic!("buffer_done should license the copy");
        };
        assert_eq!(params.width, 4);
        assert!(h.is_copying());
    }

    #[test]
    fn v1_compositor_proceeds_on_the_buffer_event_alone() {
        // The hang this state machine exists to prevent: versions below 3
        // never send `buffer_done`, so waiting for it means waiting forever.
        // The spec guarantees shm support there, making the single `buffer`
        // event the complete menu.
        for version in [1, 2] {
            let mut h = FrameHandshake::new(version);
            assert!(
                matches!(offer(&mut h), Step::SendCopy(_)),
                "v{version} must copy without waiting for buffer_done"
            );
            assert!(h.is_copying());
        }
    }

    #[test]
    fn a_v1_second_buffer_event_does_not_send_copy_twice() {
        // Copying twice on one frame is `already_used`, a fatal protocol
        // error, so the auto-close must not re-fire.
        let mut h = FrameHandshake::new(1);
        assert!(matches!(offer(&mut h), Step::SendCopy(_)));
        assert_eq!(offer(&mut h), Step::Wait);
    }

    #[test]
    fn flags_are_reported_with_ready_so_y_invert_survives() {
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer_done();
        assert_eq!(h.on_flags(1), Step::Wait);
        // Losing the flag here is exactly what makes screenshots upside-down
        // on some compositors and not others.
        assert_eq!(h.on_ready(), Step::Done { flags: 1 });
        assert!(h.is_settled());
    }

    #[test]
    fn a_missing_flags_event_means_no_flags_not_a_stall() {
        // `flags` is optional; treating its absence as "keep waiting" would
        // hang against any compositor with nothing to report.
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer_done();
        assert_eq!(h.on_ready(), Step::Done { flags: 0 });
    }

    #[test]
    fn failed_ends_the_handshake() {
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer_done();
        assert_eq!(
            h.on_failed(),
            Step::Failed(HandshakeError::CompositorFailed)
        );
        assert!(h.is_settled());
    }

    #[test]
    fn a_dmabuf_only_compositor_fails_rather_than_hanging() {
        // No `buffer` event at all: we have no shm parameters, so the only
        // honest outcome is a clean failure at menu close.
        let mut h = FrameHandshake::new(V3);
        assert_eq!(
            h.on_buffer_done(),
            Step::Failed(HandshakeError::NoUsableBuffer)
        );
    }

    #[test]
    fn an_undecodable_offer_reports_the_specific_format_it_refused() {
        let mut h = FrameHandshake::new(V3);
        h.on_buffer(9999, 4, 3, 16);
        assert_eq!(
            h.on_buffer_done(),
            Step::Failed(HandshakeError::Unusable(ParamsError::UnsupportedFormat(
                9999
            )))
        );
    }

    #[test]
    fn one_bad_offer_does_not_poison_a_good_one() {
        // v3 compositors advertise a menu; an unsupported entry alongside a
        // usable one must not abort the capture.
        let mut h = FrameHandshake::new(V3);
        h.on_buffer(9999, 4, 3, 16);
        offer(&mut h);
        assert!(matches!(h.on_buffer_done(), Step::SendCopy(_)));
    }

    #[test]
    fn the_first_usable_offer_wins_over_later_ones() {
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer(Format::Argb8888.code(), 100, 100, 400);
        let Step::SendCopy(params) = h.on_buffer_done() else {
            panic!("expected a copy");
        };
        assert_eq!(params.width, 4);
    }

    #[test]
    fn a_second_menu_close_does_not_send_copy_twice() {
        // Sending `copy` twice on one frame is the `already_used` protocol
        // error, which kills the connection.
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        assert!(matches!(h.on_buffer_done(), Step::SendCopy(_)));
        assert_eq!(h.on_buffer_done(), Step::Wait);
    }

    #[test]
    fn a_late_buffer_offer_after_copy_is_ignored() {
        // Re-allocating once the compositor is already writing would hand it
        // a buffer out from under itself.
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer_done();
        assert_eq!(h.on_buffer(Format::Argb8888.code(), 8, 8, 32), Step::Wait);
        assert!(h.is_copying());
    }

    #[test]
    fn committed_params_survive_until_ready_for_the_buffer_read() {
        let mut h = FrameHandshake::new(V3);
        assert_eq!(h.committed(), None);
        offer(&mut h);
        h.on_buffer_done();
        h.on_ready();
        // The reader needs the stride/format at `ready` time, long after the
        // `buffer` event that carried them.
        assert_eq!(h.committed().expect("committed").stride, 16);
    }

    #[test]
    fn events_after_settling_are_ignored() {
        let mut h = FrameHandshake::new(V3);
        offer(&mut h);
        h.on_buffer_done();
        h.on_ready();
        assert_eq!(h.on_ready(), Step::Wait);
        assert_eq!(h.on_failed(), Step::Wait);
    }
}
