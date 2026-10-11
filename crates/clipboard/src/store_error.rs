//! Errors produced while reading or writing clipboard history on disk.

use std::fmt::{Display, Formatter};

/// Errors produced while loading or saving history.
#[derive(Debug)]
pub enum HistoryStoreError {
    /// The filesystem rejected an operation.
    Io(std::io::Error),
    /// A file is malformed or uses an unsupported format.
    InvalidFormat(String),
}

impl HistoryStoreError {
    pub(crate) fn invalid(message: &str) -> Self {
        Self::InvalidFormat(message.to_string())
    }
}

impl Display for HistoryStoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "clipboard history I/O failed: {error}"),
            Self::InvalidFormat(message) => {
                write!(formatter, "invalid clipboard history: {message}")
            }
        }
    }
}

impl std::error::Error for HistoryStoreError {}

impl From<std::io::Error> for HistoryStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
