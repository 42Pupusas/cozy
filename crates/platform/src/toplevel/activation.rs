//! `xdg_activation_v1` -- proving to the compositor that the user asked for
//! a window to be raised.
//!
//! # The problem this solves
//!
//! A compositor refuses to raise a window on an unsolicited request, because
//! that is focus stealing. So a program that launches another program hits a
//! wall: the child asks to be raised, the compositor sees a request from a
//! process that has no focus and no evidence of user intent, and declines.
//! The user's browser opens a tab on whichever workspace it already lived
//! on, and nothing appears to happen.
//!
//! An activation token is the missing evidence. It is a chain of custody:
//!
//! 1. The launcher asks the compositor for a token, presenting the serial of
//!    the input event that caused the launch and the surface that had focus.
//!    Only a client the compositor already trusts can produce those.
//! 2. The compositor answers with an opaque string.
//! 3. The launcher passes the string to the child in `XDG_ACTIVATION_TOKEN`.
//! 4. The child presents it, and the compositor honours the raise.
//!
//! This module owns steps 1 and 2. Step 3 belongs to whoever spawns the
//! child, and step 4 belongs to the child's own toolkit.
//!
//! # Why the round trip is synchronous
//!
//! [`Self::request`] blocks until the token arrives. That is deliberate: the
//! token exists to be handed to a process we are about to spawn, and there
//! is nothing useful to do in the interval. Making it asynchronous would
//! mean holding a half-built spawn across event-loop turns for no gain, and
//! every caller would immediately have to serialise it again.
//!
//! The wait is bounded by [`Self::TOKEN_TIMEOUT`] rather than unbounded: a
//! compositor that advertises the global but never answers must degrade to
//! "launch without a token" -- the behaviour we had before this existed --
//! instead of hanging the caller forever.

use quetzalcoatl::spsc::Consumer;

use crate::conn::Sender;
use crate::error::{PlatformError, Result};
use crate::event::RawEvent;
use crate::id::{ActivationId, ActivationTokenId, DisplayId, ObjectId, SeatId, SurfaceId};
use crate::protocol::{
    ActivationRequest, ActivationTokenEvent, ActivationTokenRequest, DisplayEvent,
};
use crate::wire::{MessageBuilder, WireEvent};

/// The environment variable an activation token travels in.
///
/// Named by the xdg-activation specification. A child process reads it,
/// presents the token, and unsets it so a grandchild does not reuse a spent
/// token.
pub const TOKEN_ENV: &str = "XDG_ACTIVATION_TOKEN";

/// The evidence that a launch was the user's idea.
///
/// A token is single-use and short-lived; the compositor decides how short.
/// Hold one only for as long as it takes to spawn the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationToken(String);

impl ActivationToken {
    /// The token string, for `XDG_ACTIVATION_TOKEN`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume the token into its string.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// What a launch presents as evidence that the user asked for it.
///
/// One struct rather than four loose parameters because the compositor
/// weighs them together: the serial and the surface are the trustworthy
/// part, and separating them at the call site invites passing a serial from
/// one event with the surface of another.
#[derive(Debug, Clone, Copy)]
pub struct LaunchEvidence<'a> {
    /// The seat the input event came from.
    pub seat: SeatId,
    /// The surface that had focus when the user acted.
    pub surface: SurfaceId,
    /// The serial of the input event that caused the launch.
    pub serial: u32,
    /// The app id being launched, when the launcher knows it.
    pub app_id: Option<&'a str>,
}

/// Mints activation tokens from a bound `xdg_activation_v1`.
///
/// Its own type rather than a method on the window because the *global* is
/// optional and the exchange is self-contained: a compositor that does not
/// implement xdg-activation leaves this `None` and every caller degrades to
/// launching without a token.
#[derive(Debug, Clone, Copy)]
pub struct Activation {
    global: ActivationId,
}

impl Activation {
    /// How long to wait for the compositor's `done` event.
    ///
    /// Generous, because it is a local socket round trip and a compositor
    /// under load may take a moment. Finite, because a compositor that
    /// advertises the global and never answers must not hang a launch.
    pub const TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

    /// Build around a bound global.
    #[must_use]
    pub const fn new(global: ActivationId) -> Self {
        Self { global }
    }

    /// The bound global, for a caller that wants to `activate` directly.
    #[must_use]
    pub const fn global(&self) -> ActivationId {
        self.global
    }

    /// Ask the compositor for a token, and block until it answers.
    ///
    /// `token` is the id for the token object, supplied by the caller rather
    /// than allocated here: a window owns a reserved block of object ids, and
    /// an id drawn from anywhere else collides with one already in use.
    ///
    /// `evidence` carries the input serial and focused surface that make the
    /// token trustworthy -- see [`LaunchEvidence`].
    ///
    /// Anything popped off `events` that is not our answer is moved to
    /// `deferred` for the caller to route afterwards; see
    /// [`Self::await_done`] for why none of it may be dropped.
    ///
    /// Returns `None` when the compositor does not answer within
    /// [`Self::TOKEN_TIMEOUT`], so the caller launches unactivated rather
    /// than not at all.
    pub fn request(
        &self,
        token: ActivationTokenId,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        deferred: &mut std::collections::VecDeque<RawEvent>,
        evidence: LaunchEvidence<'_>,
    ) -> Result<Option<ActivationToken>> {
        sender.request(
            MessageBuilder::new(self.global, ActivationRequest::GetActivationToken).new_id(token),
        )?;
        sender.request(
            MessageBuilder::new(token, ActivationTokenRequest::SetSerial)
                .u32(evidence.serial)
                .object(evidence.seat),
        )?;
        if let Some(app_id) = evidence.app_id {
            sender.request(
                MessageBuilder::new(token, ActivationTokenRequest::SetAppId).string(app_id),
            )?;
        }
        sender.request(
            MessageBuilder::new(token, ActivationTokenRequest::SetSurface).object(evidence.surface),
        )?;
        sender.request(MessageBuilder::new(token, ActivationTokenRequest::Commit))?;

        let done = Self::await_done(events, deferred, token)?;

        // The token object is spent once `done` arrives, whether or not we
        // got a string out of it.
        sender.request(MessageBuilder::new(token, ActivationTokenRequest::Destroy))?;

        Ok(done.map(ActivationToken))
    }

    /// Raise the surface named by `token`.
    ///
    /// The receiving half, for a program that is handed a token rather than
    /// minting one -- a second instance of a single-instance application
    /// that must raise the window the first instance already owns.
    pub fn activate(&self, sender: &mut Sender, surface: SurfaceId, token: &str) -> Result<()> {
        sender.request(
            MessageBuilder::new(self.global, ActivationRequest::Activate)
                .string(token)
                .object(surface),
        )?;
        Ok(())
    }

    /// Pump events until our token object reports `done`, or time runs out.
    ///
    /// Every event that is not our `done` is moved to `deferred` in arrival
    /// order, for the caller to route once the exchange finishes. This
    /// consumer is the window's only source of pointer, keyboard, configure
    /// and frame events, and `RawEvent::Fd` deliveries correlate with the
    /// message before them by position. Dropping anything here would lose
    /// input, stall presentation, and break that correlation -- so nothing
    /// is dropped. A display error still aborts, because continuing past one
    /// is never correct.
    fn await_done(
        events: &mut Consumer<RawEvent>,
        deferred: &mut std::collections::VecDeque<RawEvent>,
        token: ActivationTokenId,
    ) -> Result<Option<String>> {
        let trace = Trace::from_env(token);
        let deadline = std::time::Instant::now() + Self::TOKEN_TIMEOUT;
        while std::time::Instant::now() < deadline {
            let Some(event) = events.pop() else {
                std::thread::yield_now();
                continue;
            };
            // Both variants, because the reader splits them by object: only
            // `wl_display` and `wl_registry` become `Bytes`, and everything
            // else -- our token object included -- becomes `Wire`. Reading
            // only `Bytes` would watch the two objects that never carry a
            // `done` and defer the one that does.
            let bytes = match &event {
                RawEvent::Bytes(bytes) | RawEvent::Wire { bytes, .. } => &**bytes,
                _ => {
                    deferred.push_back(event);
                    continue;
                }
            };
            let Some((mut ev, _)) = WireEvent::parse(bytes) else {
                trace.unparsed();
                deferred.push_back(event);
                continue;
            };
            trace.saw(ev.object_id, ev.opcode);

            if ev.object_id == DisplayId(1).raw()
                && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
            {
                return Err(PlatformError::Protocol {
                    object: ev.u32(),
                    code: ev.u32(),
                    message: ev.string(),
                });
            }

            if ev.object_id == token.raw()
                && ActivationTokenEvent::from_opcode(ev.opcode) == Some(ActivationTokenEvent::Done)
            {
                return Ok(Some(ev.string()));
            }
            deferred.push_back(event);
        }
        trace.timed_out();
        Ok(None)
    }
}

/// Prints what arrives during a token exchange, when `COZY_DEBUG_ACTIVATION`
/// is set.
///
/// The exchange fails silently by design -- a timeout is indistinguishable
/// from a compositor that never answers -- so this reports the object ids and
/// opcodes actually seen, which is the difference between "nothing arrived"
/// and "the answer arrived and we did not recognise it".
struct Trace {
    enabled: bool,
    token: ActivationTokenId,
}

impl Trace {
    const ENV: &'static str = "COZY_DEBUG_ACTIVATION";

    fn from_env(token: ActivationTokenId) -> Self {
        Self {
            enabled: std::env::var_os(Self::ENV).is_some(),
            token,
        }
    }

    fn saw(&self, object: u32, opcode: u16) {
        if self.enabled {
            eprintln!(
                "activation: event object={object} opcode={opcode} (awaiting object={} opcode=0)",
                self.token.raw()
            );
        }
    }

    fn unparsed(&self) {
        if self.enabled {
            eprintln!("activation: unparsable event");
        }
    }

    fn timed_out(&self) {
        if self.enabled {
            eprintln!(
                "activation: no done for object={} within {:?}",
                self.token.raw(),
                Activation::TOKEN_TIMEOUT
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_exposes_its_string() {
        let token = ActivationToken("abc123".to_owned());
        assert_eq!(token.as_str(), "abc123");
        assert_eq!(token.into_string(), "abc123");
    }

    /// The variable name is specified, not ours to choose: a child looks for
    /// exactly this and nothing else.
    #[test]
    fn the_token_environment_variable_is_the_specified_name() {
        assert_eq!(TOKEN_ENV, "XDG_ACTIVATION_TOKEN");
    }

    /// The timeout must be finite, or a compositor that advertises the
    /// global and never answers hangs every launch.
    #[test]
    fn the_token_timeout_is_bounded() {
        assert!(Activation::TOKEN_TIMEOUT > std::time::Duration::ZERO);
        assert!(Activation::TOKEN_TIMEOUT <= std::time::Duration::from_secs(1));
    }

    #[test]
    fn an_activation_keeps_the_global_it_was_built_from() {
        assert_eq!(Activation::new(ActivationId(7)).global(), ActivationId(7));
    }

    /// With no events to read, the wait ends in `None` rather than blocking
    /// forever -- the degradation that keeps a launch working on a
    /// compositor that goes quiet.
    #[test]
    fn a_silent_compositor_yields_no_token() {
        let (_producer, mut consumer) =
            quetzalcoatl::spsc::RingBuffer::<RawEvent>::new(quetzalcoatl::capacity::Capacity::exact(
                4,
            ))
            .split();
        let started = std::time::Instant::now();
        let mut deferred = std::collections::VecDeque::new();
        let got = Activation::await_done(&mut consumer, &mut deferred, ActivationTokenId(9))
            .expect("no error");
        assert!(got.is_none());
        assert!(started.elapsed() >= Activation::TOKEN_TIMEOUT);
    }

    /// The regression that wedged the file browser: this consumer is the
    /// window's *only* source of pointer, keyboard, configure and frame
    /// events. Dropping what we pop while waiting loses user input and
    /// stalls presentation until the reader's progress guard gives up.
    #[test]
    fn events_that_are_not_ours_are_kept_for_the_caller() {
        let (producer, mut consumer) =
            quetzalcoatl::spsc::RingBuffer::<RawEvent>::new(quetzalcoatl::capacity::Capacity::exact(
                8,
            ))
            .split();
        // Not object 1: that is wl_display, whose opcode 0 is `error`.
        for object in 41u32..=43 {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&object.to_ne_bytes());
            bytes.extend_from_slice(&(8u32 << 16).to_ne_bytes());
            producer
                .push_block(RawEvent::Bytes(bytes.into()))
                .expect("consumer alive");
        }

        let mut deferred = std::collections::VecDeque::new();
        let got = Activation::await_done(&mut consumer, &mut deferred, ActivationTokenId(99))
            .expect("no error");

        assert!(got.is_none());
        assert_eq!(deferred.len(), 3, "every foreign event must be preserved");
    }

    /// Builds the compositor's side of the exchange, so a test can feed the
    /// wait exactly what niri sends.
    struct Compositor;

    impl Compositor {
        /// Encode `xdg_activation_token_v1.done` carrying `token_string`.
        ///
        /// `done` is opcode 0, which is `SetSerial`'s number on the request
        /// side -- the builder writes whichever opcode it is handed, and the
        /// two enums are separate namespaces on the wire.
        fn done(token: ActivationTokenId, token_string: &str) -> Vec<u8> {
            MessageBuilder::new(token, ActivationTokenRequest::SetSerial)
                .string(token_string)
                .finish()
        }
    }

    /// The regression that made every launch tokenless: the reader turns a
    /// message into `Bytes` only for `wl_display` and `wl_registry`, and into
    /// `Wire` for every other object. A token object is one of those others,
    /// so its `done` arrives as `Wire`, and a wait that reads only `Bytes`
    /// never sees the answer it asked for.
    #[test]
    fn a_done_arriving_as_a_wire_event_is_recognised() {
        let (producer, mut consumer) =
            quetzalcoatl::spsc::RingBuffer::<RawEvent>::new(quetzalcoatl::capacity::Capacity::exact(
                4,
            ))
            .split();
        let token = ActivationTokenId(15);
        producer
            .push_block(RawEvent::from_wire_msg(&Compositor::done(token, "tok-xyz")))
            .expect("consumer alive");

        let mut deferred = std::collections::VecDeque::new();
        let got = Activation::await_done(&mut consumer, &mut deferred, token).expect("no error");

        assert_eq!(got.as_deref(), Some("tok-xyz"));
        assert!(deferred.is_empty(), "the answer must not be deferred");
    }

    /// A deferred event must keep its place: a `RawEvent::Fd` is correlated
    /// with the message before it by position alone, so reordering hands a
    /// keymap fd to the wrong event.
    #[test]
    fn deferred_events_keep_their_arrival_order() {
        let (producer, mut consumer) =
            quetzalcoatl::spsc::RingBuffer::<RawEvent>::new(quetzalcoatl::capacity::Capacity::exact(
                8,
            ))
            .split();
        for object in [11u32, 22, 33] {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&object.to_ne_bytes());
            bytes.extend_from_slice(&(8u32 << 16).to_ne_bytes());
            producer
                .push_block(RawEvent::Bytes(bytes.into()))
                .expect("consumer alive");
        }

        let mut deferred = std::collections::VecDeque::new();
        Activation::await_done(&mut consumer, &mut deferred, ActivationTokenId(99))
            .expect("no error");

        let seen: Vec<u32> = deferred
            .iter()
            .filter_map(|ev| match ev {
                RawEvent::Bytes(b) => WireEvent::parse(b).map(|(e, _)| e.object_id),
                _ => None,
            })
            .collect();
        assert_eq!(seen, vec![11, 22, 33]);
    }
}
