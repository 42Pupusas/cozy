//! `xdg_activation_v1`: carries "the user asked for this" across a process
//! boundary so the compositor honours a raise instead of treating it as
//! focus stealing.
//!
//! The launcher asks for a token with the triggering input serial and the
//! focused surface, then hands the token string to the child in
//! `XDG_ACTIVATION_TOKEN`; the child presents it with `activate`. A token is
//! single-use and short-lived.

use super::Interface;

pub const XDG_ACTIVATION_V1: Interface = Interface {
    name: "xdg_activation_v1",
    version: 1,
};

requests! {
    /// `xdg_activation_v1` requests — the token factory.
    ActivationRequest {
        Destroy = 0,
        /// Create a token object to configure and then commit.
        GetActivationToken = 1,
        /// Raise the surface associated with a token string.
        Activate = 2,
    }
}

requests! {
    /// `xdg_activation_token_v1` requests — one token under construction.
    ///
    /// `SetSerial` and `SetSurface` are what make the token trustworthy:
    /// they tell the compositor which input event and which focused surface
    /// the launch came from. A token committed without them is still issued,
    /// but a compositor is free to treat it as weak evidence.
    ActivationTokenRequest {
        /// The input event serial and the seat it came from.
        SetSerial = 0,
        /// The app id being launched.
        SetAppId = 1,
        /// The surface that was focused when the user acted.
        SetSurface = 2,
        /// Ask for the token string; answered by `ActivationTokenEvent::Done`.
        Commit = 3,
        Destroy = 4,
    }
}
events! {
    /// `xdg_activation_token_v1` events.
    ActivationTokenEvent {
        /// The token string. Single-use, and the object is spent after it.
        Done = 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn activation_opcodes_match_the_protocol_xml() {
        assert_eq!(ActivationRequest::Destroy.opcode(), 0);
        assert_eq!(ActivationRequest::GetActivationToken.opcode(), 1);
        assert_eq!(ActivationRequest::Activate.opcode(), 2);

        assert_eq!(ActivationTokenRequest::SetSerial.opcode(), 0);
        assert_eq!(ActivationTokenRequest::SetAppId.opcode(), 1);
        assert_eq!(ActivationTokenRequest::SetSurface.opcode(), 2);
        assert_eq!(ActivationTokenRequest::Commit.opcode(), 3);
        assert_eq!(ActivationTokenRequest::Destroy.opcode(), 4);

        assert_eq!(
            ActivationTokenEvent::from_opcode(0),
            Some(ActivationTokenEvent::Done)
        );
        assert_eq!(ActivationTokenEvent::from_opcode(1), None);
    }
}
