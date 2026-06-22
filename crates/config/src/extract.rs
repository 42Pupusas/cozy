//! Tiny helpers that walk a shelby `Table` and extract typed values.
//! Keeps `lib.rs` clean — no shelby types leak into the public API.

use shelby::{Table, Value};

use crate::ConfigError;

/// Get a sub-table by key, or `None` if missing / not a table.
pub fn table<'t>(table: &'t Table<'_>, key: &str) -> Option<&'t Table<'t>> {
    match table.get(key)? {
        Value::Table(t) => Some(t),
        _ => None,
    }
}

/// Get a string value by key, or `None` if missing / not a string.
pub fn str<'t>(table: &'t Table<'_>, key: &str) -> Option<&'t str> {
    match table.get(key)? {
        Value::String(s) => Some(s.as_ref()),
        _ => None,
    }
}

/// Get a boolean value by key, or `None` if missing / not a boolean.
pub fn bool(table: &Table<'_>, key: &str) -> Option<bool> {
    match table.get(key)? {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

/// Read an integer in `[lo, hi]`, returning a typed validation error otherwise.
/// `key` is the table-relative lookup key; `field` is the qualified name used
/// in error messages (e.g. `"font.size"`). Returns `Ok(None)` when absent.
pub fn int_range(
    table:  &Table<'_>,
    key:    &'static str,
    field:  &'static str,
    lo:     i64,
    hi:     i64,
    reason: &'static str,
) -> Result<Option<i64>, ConfigError> {
    match table.get(key) {
        None => Ok(None),
        Some(Value::Integer(n)) if (lo..=hi).contains(n) => Ok(Some(*n)),
        Some(Value::Integer(_)) => Err(ConfigError::InvalidValue { field, reason }),
        Some(_) => Err(ConfigError::InvalidValue { field, reason: "expected integer" }),
    }
}
