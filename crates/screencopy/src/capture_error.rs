//! This crate's error type: a capture failure, or the platform layer's.
//!
//! Deliberately *not* a new variant on `platform::PlatformError`. Platform is
//! the lower layer; teaching it about screencopy would mean every consumer of
//! a terminal window carries a capture-shaped error case it can never hit,
//! and would invert the dependency the workspace keeps strictly one-way (see
//! `platform::error`'s own note about `SysError` converting up one layer at a
//! time rather than skipping). So screencopy wraps platform, never the
//! reverse.

use platform::PlatformError;
use platform::conn::ConnError;

use crate::captured_frame::CaptureError as ConversionError;
use crate::frame_handshake::HandshakeError;

/// Anything that can stop a capture from producing pixels.
#[derive(Debug)]
pub enum ScreencopyError {
    /// The Wayland connection or protocol failed underneath us.
    Platform(PlatformError),
    /// The capture negotiation ended without pixels.
    Handshake(HandshakeError),
    /// The compositor's bytes couldn't be turned into an image.
    Conversion(ConversionError),
    /// The compositor reported success but the session had no buffer to read
    /// -- a self-check on our own state machine rather than a compositor
    /// fault, so it is kept distinct from the cases above.
    NoBuffer,
}

impl std::fmt::Display for ScreencopyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Platform(e) => write!(f, "{e}"),
            Self::Handshake(e) => write!(f, "{e}"),
            Self::Conversion(e) => write!(f, "{e}"),
            Self::NoBuffer => {
                write!(f, "capture reported ready but no buffer was allocated")
            }
        }
    }
}

impl std::error::Error for ScreencopyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Platform(e) => Some(e),
            Self::Handshake(e) => Some(e),
            Self::Conversion(e) => Some(e),
            Self::NoBuffer => None,
        }
    }
}

impl From<PlatformError> for ScreencopyError {
    fn from(e: PlatformError) -> Self {
        Self::Platform(e)
    }
}

impl From<ConnError> for ScreencopyError {
    /// Socket failures (what `Sender::request` returns) arrive as `ConnError`.
    ///
    /// Routed through `PlatformError::Conn` rather than given their own
    /// variant, so the error chain stays the same strict one-way ladder
    /// platform already documents: `SysError -> ConnError -> Error ->
    /// ScreencopyError`, with no layer-skipping shortcut.
    fn from(e: ConnError) -> Self {
        Self::Platform(PlatformError::Conn(e))
    }
}

impl From<HandshakeError> for ScreencopyError {
    fn from(e: HandshakeError) -> Self {
        Self::Handshake(e)
    }
}

impl From<ConversionError> for ScreencopyError {
    fn from(e: ConversionError) -> Self {
        Self::Conversion(e)
    }
}

/// Convenience alias for this crate's fallible operations.
pub type Result<T> = std::result::Result<T, ScreencopyError>;

#[cfg(test)]
mod tests {
    use super::ScreencopyError;
    use crate::captured_frame::CaptureError as ConversionError;
    use crate::frame_handshake::HandshakeError;
    use crate::frame_params::ParamsError;
    use std::error::Error as _;

    #[test]
    fn display_delegates_to_the_wrapped_error() {
        // The wrapper must not bury the specific reason -- "capture failed"
        // alone would tell a user nothing about which compositor format was
        // refused.
        let inner = HandshakeError::Unusable(ParamsError::UnsupportedFormat(9999));
        let msg = inner.to_string();
        assert_eq!(ScreencopyError::Handshake(inner).to_string(), msg);
    }

    #[test]
    fn wrapped_errors_are_reachable_as_sources() {
        let e = ScreencopyError::Conversion(ConversionError::ShortBuffer {
            needed: 64,
            actual: 32,
        });
        assert!(e.source().is_some());
        assert!(ScreencopyError::NoBuffer.source().is_none());
    }

    #[test]
    fn socket_errors_climb_the_ladder_rather_than_skipping_a_layer() {
        // `?` on a `Sender::request` must land here without screencopy
        // inventing a parallel path around `platform::PlatformError`.
        let e: ScreencopyError = platform::conn::ConnError::Closed.into();
        assert!(matches!(
            e,
            ScreencopyError::Platform(platform::PlatformError::Conn(_))
        ));
    }

    #[test]
    fn platform_errors_convert_upward_with_the_question_mark_operator() {
        // Screencopy wraps platform, never the other way round; this is the
        // conversion that keeps the dependency one-way.
        let e: ScreencopyError = platform::PlatformError::ReaderStopped.into();
        assert!(matches!(e, ScreencopyError::Platform(_)));
    }
}
