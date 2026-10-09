//! Top-level error type for `crates/platform`.
//!
//! Stripped-down version of paint's error.rs — no EGL proxy.
//!
//! Named `PlatformError` (not the bare `Error` every sibling error type in
//! this workspace deliberately avoids -- `ConnError`, `SysError`,
//! `DbusError`, `ConfigError`, `ScreencopyError`, and so on) because a bare
//! `Error` collides, at the level of plain text, with the ubiquitous
//! `impl std::error::Error for X` / `dyn std::error::Error` idiom every one
//! of those types also writes. `cargo graph` matches nodes by bare type
//! name, so a bare-named `Error` here was being misread as a reference to
//! *this* type from every unrelated crate that implements the standard
//! `Error` trait, manufacturing a false `SysError -> ConnError -> Error ->
//! ConnError` cycle where the real dependency is a strict one-way ladder.
//! Renaming removes the ambiguity at the source instead of teaching the
//! graph tool to special-case one name.

use std::fmt;
use std::io;

use crate::conn::ConnError;

/// Anything that can go wrong opening or running the terminal window.
#[derive(Debug)]
pub enum PlatformError {
    Conn(ConnError),
    Protocol {
        object: u32,
        code: u32,
        message: String,
    },
    ReaderStopped,
    ReaderPanicked,
    ReaderSpawn(io::Error),
    MissingGlobal(&'static str),
}

impl fmt::Display for PlatformError {
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
            Self::MissingGlobal(name) => write!(f, "compositor does not offer {name}"),
        }
    }
}

impl std::error::Error for PlatformError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conn(e) => Some(e),
            Self::ReaderSpawn(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ConnError> for PlatformError {
    fn from(e: ConnError) -> Self {
        Self::Conn(e)
    }
}

pub type Result<T> = std::result::Result<T, PlatformError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_protocol_error_includes_all_fields() {
        let e = PlatformError::Protocol {
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
            PlatformError::ReaderStopped.to_string(),
            "reader thread stopped unexpectedly"
        );
        assert_eq!(
            PlatformError::ReaderPanicked.to_string(),
            "reader thread panicked"
        );
    }

    #[test]
    fn display_missing_global_names_the_interface() {
        assert_eq!(
            PlatformError::MissingGlobal("wp_viewporter").to_string(),
            "compositor does not offer wp_viewporter"
        );
    }

    #[test]
    fn display_reader_spawn_includes_inner_io_error() {
        let inner = io::Error::other("no threads");
        let e = PlatformError::ReaderSpawn(inner);
        assert_eq!(e.to_string(), "failed to spawn reader thread: no threads");
    }

    #[test]
    fn display_conn_delegates_verbatim_to_inner_conn_error() {
        let inner = ConnError::Closed;
        let msg = inner.to_string();
        let e = PlatformError::Conn(inner);
        assert_eq!(e.to_string(), msg);
    }

    #[test]
    fn source_present_only_for_conn_and_reader_spawn() {
        use std::error::Error as _;
        assert!(PlatformError::Conn(ConnError::Closed).source().is_some());
        assert!(
            PlatformError::ReaderSpawn(io::Error::other("x"))
                .source()
                .is_some()
        );
        assert!(PlatformError::ReaderStopped.source().is_none());
        assert!(PlatformError::ReaderPanicked.source().is_none());
        assert!(
            PlatformError::Protocol {
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
        let e: PlatformError = ConnError::Closed.into();
        assert!(matches!(e, PlatformError::Conn(ConnError::Closed)));
    }

    #[test]
    fn sys_error_reaches_error_only_through_the_conn_layer() {
        // `SysError` has no direct `From` into `PlatformError`; it converts
        // up one layer at a time (`SysError -> ConnError -> PlatformError`),
        // keeping the error dependency a strict chain rather than a
        // layer-skipping cycle.
        use crate::sys::SysError;
        let inner = SysError::Memfd(io::Error::other("x"));
        let msg = inner.to_string();
        let conn: ConnError = inner.into();
        let e: PlatformError = conn.into();
        match e {
            PlatformError::Conn(ConnError::Sys(s)) => assert_eq!(s.to_string(), msg),
            other => panic!("expected PlatformError::Conn(ConnError::Sys(_)), got {other:?}"),
        }
    }
}
