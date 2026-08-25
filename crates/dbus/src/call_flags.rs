//! The flags byte of a `METHOD_CALL` header.
//!
//! Only one of these matters to this workspace, and it matters a great deal:
//! `ALLOW_INTERACTIVE_AUTHORIZATION`.
//!
//! A polkit-guarded method whose policy says `auth_admin` has two possible
//! behaviours, and the *caller* chooses between them with this flag:
//!
//! - Without it, the service refuses immediately with
//!   `InteractiveAuthorizationRequired`. No agent is consulted and the user
//!   sees nothing.
//! - With it, polkit asks the session's authentication agent for a password,
//!   and the call blocks until the user answers.
//!
//! So an agent alone is not enough to make mounting work: an agent can be
//! registered, correct, and never called, simply because the request that
//! needed it did not permit a prompt. That happened here -- the agent
//! registered, the mount was attempted, and no prompt ever arrived.

/// Flags for an outgoing method call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CallFlags {
    bits: u8,
}

impl CallFlags {
    /// `NO_REPLY_EXPECTED`.
    const NO_REPLY_EXPECTED: u8 = 0x1;
    /// `NO_AUTO_START`.
    const NO_AUTO_START: u8 = 0x2;
    /// `ALLOW_INTERACTIVE_AUTHORIZATION`.
    const ALLOW_INTERACTIVE_AUTHORIZATION: u8 = 0x4;

    /// The default: expect a reply, allow auto-start, refuse to prompt.
    #[must_use]
    pub const fn none() -> Self {
        Self { bits: 0 }
    }

    /// Permit the service to ask the user for authentication rather than
    /// refusing outright.
    #[must_use]
    pub const fn allow_interactive_authorization() -> Self {
        Self {
            bits: Self::ALLOW_INTERACTIVE_AUTHORIZATION,
        }
    }

    /// Do not send a reply.
    #[must_use]
    pub const fn no_reply_expected(self) -> Self {
        Self {
            bits: self.bits | Self::NO_REPLY_EXPECTED,
        }
    }

    /// Do not activate the destination if it is not running.
    #[must_use]
    pub const fn no_auto_start(self) -> Self {
        Self {
            bits: self.bits | Self::NO_AUTO_START,
        }
    }

    /// Also allow an interactive prompt.
    #[must_use]
    pub const fn with_interactive_authorization(self) -> Self {
        Self {
            bits: self.bits | Self::ALLOW_INTERACTIVE_AUTHORIZATION,
        }
    }

    /// The byte as it appears in the message header.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.bits
    }

    /// Whether a prompt is permitted.
    #[must_use]
    pub const fn allows_interactive_authorization(self) -> bool {
        self.bits & Self::ALLOW_INTERACTIVE_AUTHORIZATION != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default must stay zero, which is what every existing caller in
    /// this workspace already sends on the wire.
    #[test]
    fn the_default_is_a_zero_byte() {
        assert_eq!(CallFlags::none().as_byte(), 0);
        assert_eq!(CallFlags::default().as_byte(), 0);
    }

    /// The bit value is fixed by the D-Bus specification; getting it wrong
    /// means either no prompt or a nonsense flag.
    #[test]
    fn interactive_authorization_is_bit_two() {
        assert_eq!(CallFlags::allow_interactive_authorization().as_byte(), 0x4);
    }

    /// The flag must be readable back, since that is how a caller checks it
    /// asked for a prompt.
    #[test]
    fn the_interactive_flag_reads_back() {
        assert!(
            CallFlags::allow_interactive_authorization().allows_interactive_authorization()
        );
        assert!(!CallFlags::none().allows_interactive_authorization());
    }

    /// The flags compose without clobbering each other.
    #[test]
    fn flags_combine_without_losing_each_other() {
        let flags = CallFlags::none()
            .no_auto_start()
            .with_interactive_authorization();
        assert_eq!(flags.as_byte(), 0x2 | 0x4);
        assert!(flags.allows_interactive_authorization());
    }

    /// Every documented bit has its specified value.
    #[test]
    fn each_flag_has_its_specified_bit() {
        assert_eq!(CallFlags::none().no_reply_expected().as_byte(), 0x1);
        assert_eq!(CallFlags::none().no_auto_start().as_byte(), 0x2);
    }
}
