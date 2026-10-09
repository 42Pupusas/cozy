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
//! This module owns the request half of step 1. The answer of step 2 is
//! collected by [`crate::toplevel::activation_queue::ActivationQueue`], and
//! step 3 belongs to whoever spawns the child.
//!
//! # Why the exchange is not awaited
//!
//! [`Activation::begin`] sends the request and returns. The `done` event
//! arrives through the window's ordinary event routing, like every other
//! compositor event, and the caller spawns the child when it does. Nothing
//! blocks the event loop, nothing pops events out of order, and no timeout is
//! needed: the loop is woken by the reader when the answer arrives.

use crate::conn::Sender;
use crate::error::Result;
use crate::id::{ActivationId, ActivationTokenId, SeatId, SurfaceId};
use crate::protocol::{ActivationRequest, ActivationTokenRequest};
use crate::wire::MessageBuilder;

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
    pub(crate) const fn new(token: String) -> Self {
        Self(token)
    }

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

    /// Ask the compositor for a token and return without waiting.
    ///
    /// `token` is the id for the token object, supplied by the caller rather
    /// than allocated here: a window owns a reserved block of object ids, and
    /// an id drawn from anywhere else collides with one already in use.
    ///
    /// `evidence` carries the input serial and focused surface that make the
    /// token trustworthy -- see [`LaunchEvidence`]. The answer arrives later
    /// as a `done` event on `token`.
    pub fn begin(
        &self,
        token: ActivationTokenId,
        sender: &mut Sender,
        evidence: LaunchEvidence<'_>,
    ) -> Result<()> {
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
        Ok(())
    }

    /// Destroy a token object once its `done` has been read.
    pub fn release(sender: &mut Sender, token: ActivationTokenId) -> Result<()> {
        sender.request(MessageBuilder::new(token, ActivationTokenRequest::Destroy))?;
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_exposes_its_string() {
        let token = ActivationToken::new("abc123".to_owned());
        assert_eq!(token.as_str(), "abc123");
        assert_eq!(token.into_string(), "abc123");
    }

    /// The variable name is specified, not ours to choose: a child looks for
    /// exactly this and nothing else.
    #[test]
    fn the_token_environment_variable_is_the_specified_name() {
        assert_eq!(TOKEN_ENV, "XDG_ACTIVATION_TOKEN");
    }

    #[test]
    fn an_activation_keeps_the_global_it_was_built_from() {
        assert_eq!(Activation::new(ActivationId(7)).global(), ActivationId(7));
    }
}
