//! Tiny helper that walks a shelby `Table` and extracts typed values.
//! Keeps `lib.rs` clean — no shelby types leak into the public API.

use shelby::{Table, Value};

use crate::ConfigError;

pub(crate) struct Extractor<'b> {
    _bytes: &'b [u8],
}

impl<'b> Extractor<'b> {
    pub(crate) const fn new(bytes: &'b [u8]) -> Self {
        Self { _bytes: bytes }
    }

    /// Get a sub-table by key, or `None` if missing.
    pub(crate) fn table<'t>(
        &self,
        table: &'t Table<'_>,
        key: &str,
    ) -> Option<&'t Table<'t>> {
        match table.get(key)? {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    /// Get a string value by key, or `None` if missing.
    pub(crate) fn str<'t>(
        &self,
        table: &'t Table<'_>,
        key: &str,
    ) -> Option<&'t str> {
        match table.get(key)? {
            Value::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// Get an integer value by key. Returns `Ok(None)` if missing,
    /// `Err(...)` if present but not an integer.
    pub(crate) fn int(
        &self,
        table: &Table<'_>,
        key: &'static str,
    ) -> Result<Option<i64>, ConfigError> {
        match table.get(key) {
            None => Ok(None),
            Some(Value::Integer(n)) => Ok(Some(*n)),
            Some(_) => Err(ConfigError::InvalidValue { field: key, reason: "expected integer" }),
        }
    }

    /// Get a boolean value by key, or `None` if missing.
    pub(crate) fn bool(
        &self,
        table: &Table<'_>,
        key: &str,
    ) -> Option<bool> {
        match table.get(key)? {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }
}
