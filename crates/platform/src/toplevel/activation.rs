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
    /// `serial` is the input event that triggered the launch and `surface`
    /// is the surface that had focus when it happened. Both are what make
    /// the token trustworthy: they are the compositor's own evidence that a
    /// user acted, replayed back to it. `app_id` names the program being
    /// launched, which lets the compositor apply the right window rule to a
    /// window that does not exist yet.
    ///
    /// Returns `None` when the compositor does not answer within
    /// [`Self::TOKEN_TIMEOUT`], so the caller launches unactivated rather
    /// than not at all.
    pub fn request(
        &self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        seat: SeatId,
        surface: SurfaceId,
        serial: u32,
        app_id: &str,
    ) -> Result<Option<ActivationToken>> {
        let token: ActivationTokenId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(self.global, ActivationRequest::GetActivationToken).new_id(token),
        )?;
        sender.request(
            MessageBuilder::new(token, ActivationTokenRequest::SetSerial)
                .u32(serial)
                .object(seat),
        )?;
        sender.request(
            MessageBuilder::new(token, ActivationTokenRequest::SetAppId).string(app_id),
        )?;
        sender.request(
            MessageBuilder::new(token, ActivationTokenRequest::SetSurface).object(surface),
        )?;
        sender.request(MessageBuilder::new(token, ActivationTokenRequest::Commit))?;

        let done = Self::await_done(events, token)?;

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
    /// Events for other objects are dropped rather than requeued. This runs
    /// during a launch, between event-loop turns, so the only traffic worth
    /// keeping is the answer we asked for; a display error still aborts,
    /// because continuing past one is never correct.
    fn await_done(
        events: &mut Consumer<RawEvent>,
        token: ActivationTokenId,
    ) -> Result<Option<String>> {
        let deadline = std::time::Instant::now() + Self::TOKEN_TIMEOUT;
        while std::time::Instant::now() < deadline {
            let Some(RawEvent::Bytes(bytes)) = events.pop() else {
                std::thread::yield_now();
                continue;
            };
            let Some((mut ev, _)) = WireEvent::parse(&bytes) else {
                continue;
            };

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
        }
        Ok(None)
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
        let got = Activation::await_done(&mut consumer, ActivationTokenId(9)).expect("no error");
        assert!(got.is_none());
        assert!(started.elapsed() >= Activation::TOKEN_TIMEOUT);
    }
}
