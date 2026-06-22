//! Top-level error type for `crates/platform`.
//!
//! Stripped-down version of paint's error.rs — no EGL proxy.

use std::fmt;
use std::io;

use crate::conn::ConnError;
use crate::sys::SysError;

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
            Error::Conn(e) => write!(f, "{e}"),
            Error::Protocol { object, code, message } => {
                write!(f, "wayland protocol error on object {object} (code {code}): {message}")
            }
            Error::ReaderStopped => write!(f, "reader thread stopped unexpectedly"),
            Error::ReaderPanicked => write!(f, "reader thread panicked"),
            Error::ReaderSpawn(e) => write!(f, "failed to spawn reader thread: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Conn(e) => Some(e),
            Error::ReaderSpawn(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ConnError> for Error {
    fn from(e: ConnError) -> Self {
        Error::Conn(e)
    }
}

impl From<SysError> for Error {
    fn from(e: SysError) -> Self {
        Error::Conn(ConnError::Sys(e))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
