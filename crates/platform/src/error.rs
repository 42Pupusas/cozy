//! Top-level error type for `crates/platform`.
//!
//! Stripped-down version of paint's error.rs — no EGL proxy.

use std::fmt;
use std::io;

use crate::conn::ConnError;

/// Anything that can go wrong opening or running the terminal window.
#[derive(Debug)]
pub enum Error {
    Conn(ConnError),
    Protocol {
        object: u32,
        code: u32,
        message: String,
    },
    ReaderStopped,
    ReaderPanicked,
    ReaderSpawn(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conn(e) => write!(f, "{e}"),
            Self::Protocol {
                object,
                code,
                message,
            } => {
                write!(
                    f,
                    "wayland protocol error on object {object} (code {code}): {message}"
                )
            }
            Self::ReaderStopped => write!(f, "reader thread stopped unexpectedly"),
            Self::ReaderPanicked => write!(f, "reader thread panicked"),
            Self::ReaderSpawn(e) => write!(f, "failed to spawn reader thread: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conn(e) => Some(e),
            Self::ReaderSpawn(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ConnError> for Error {
    fn from(e: ConnError) -> Self {
        Self::Conn(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_protocol_error_includes_all_fields() {
        let e = Error::Protocol {
            object: 7,
            code: 2,
            message: "oops".into(),
        };
        assert_eq!(
            e.to_string(),
            "wayland protocol error on object 7 (code 2): oops"
        );
    }

    #[test]
    fn display_reader_variants_have_fixed_messages() {
        assert_eq!(
            Error::ReaderStopped.to_string(),
            "reader thread stopped unexpectedly"
        );
        assert_eq!(Error::ReaderPanicked.to_string(), "reader thread panicked");
    }

    #[test]
    fn display_reader_spawn_includes_inner_io_error() {
        let inner = io::Error::other("no threads");
        let e = Error::ReaderSpawn(inner);
        assert_eq!(e.to_string(), "failed to spawn reader thread: no threads");
    }

    #[test]
    fn display_conn_delegates_verbatim_to_inner_conn_error() {
        let inner = ConnError::Closed;
        let msg = inner.to_string();
        let e = Error::Conn(inner);
        assert_eq!(e.to_string(), msg);
    }

    #[test]
    fn source_present_only_for_conn_and_reader_spawn() {
        use std::error::Error as _;
        assert!(Error::Conn(ConnError::Closed).source().is_some());
        assert!(Error::ReaderSpawn(io::Error::other("x")).source().is_some());
        assert!(Error::ReaderStopped.source().is_none());
        assert!(Error::ReaderPanicked.source().is_none());
        assert!(
            Error::Protocol {
                object: 0,
                code: 0,
                message: String::new()
            }
            .source()
            .is_none()
        );
    }

    #[test]
    fn from_conn_error_wraps_as_conn_variant() {
        let e: Error = ConnError::Closed.into();
        assert!(matches!(e, Error::Conn(ConnError::Closed)));
    }

    #[test]
    fn sys_error_reaches_error_only_through_the_conn_layer() {
        // `SysError` has no direct `From` into `Error`; it converts up one
        // layer at a time (`SysError -> ConnError -> Error`), keeping the
        // error dependency a strict chain rather than a layer-skipping cycle.
        use crate::sys::SysError;
        let inner = SysError::Memfd(io::Error::other("x"));
        let msg = inner.to_string();
        let conn: ConnError = inner.into();
        let e: Error = conn.into();
        match e {
            Error::Conn(ConnError::Sys(s)) => assert_eq!(s.to_string(), msg),
            other => panic!("expected Error::Conn(ConnError::Sys(_)), got {other:?}"),
        }
    }
}
