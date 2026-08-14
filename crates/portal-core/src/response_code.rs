//! The `response` field every `Request::Response` signal carries: the
//! three numeric codes the spec defines for how a user interaction ended.
//!
//! An enum rather than a bare `u32` because these three values are the
//! entire domain -- a backend that answers `3` is answering nothing any
//! caller understands -- and because the two "not success" codes are
//! genuinely different outcomes that dialog code has to choose between:
//! `Cancelled` is the user deciding, `Other` is the interaction ending
//! without a decision (the caller closed the request, the session died).
//! Keeping them apart here means the choice happens once, in domain code
//! with tests, rather than as a literal at each call site.

use dbus::value::Value;

/// How a portal request's user interaction ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseCode {
    /// The request was carried out.
    Success,
    /// The user cancelled the interaction.
    Cancelled,
    /// The interaction ended in some other way.
    Other,
}

impl ResponseCode {
    /// The wire number for this code.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Success => 0,
            Self::Cancelled => 1,
            Self::Other => 2,
        }
    }

    /// Parse a wire number. Anything outside the three documented codes is
    /// not a response this backend can mean, so it decodes to `None`
    /// rather than silently becoming `Other`.
    #[must_use]
    pub const fn from_u32(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Success),
            1 => Some(Self::Cancelled),
            2 => Some(Self::Other),
            _ => None,
        }
    }

    /// The `u` value a `Response` signal body starts with.
    #[must_use]
    pub const fn as_value(self) -> Value {
        Value::U32(self.as_u32())
    }

    /// Whether the request was carried out, i.e. whether the `results`
    /// vardict beside this code is meaningful.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_code_carries_its_documented_wire_number() {
        assert_eq!(ResponseCode::Success.as_u32(), 0);
        assert_eq!(ResponseCode::Cancelled.as_u32(), 1);
        assert_eq!(ResponseCode::Other.as_u32(), 2);
    }

    #[test]
    fn every_code_round_trips_through_its_wire_number() {
        for code in [
            ResponseCode::Success,
            ResponseCode::Cancelled,
            ResponseCode::Other,
        ] {
            assert_eq!(ResponseCode::from_u32(code.as_u32()), Some(code));
        }
    }

    #[test]
    fn an_undocumented_number_decodes_to_nothing() {
        assert_eq!(ResponseCode::from_u32(3), None);
        assert_eq!(ResponseCode::from_u32(u32::MAX), None);
    }

    #[test]
    fn only_success_reports_success() {
        assert!(ResponseCode::Success.is_success());
        assert!(!ResponseCode::Cancelled.is_success());
        assert!(!ResponseCode::Other.is_success());
    }

    #[test]
    fn the_signal_value_is_an_unsigned_int() {
        assert_eq!(ResponseCode::Cancelled.as_value(), Value::U32(1));
    }
}
