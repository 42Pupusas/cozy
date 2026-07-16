//! An extension trait for typed reads off a shelby [`Table`].
//!
//! `Table` is defined in an external crate, so we can't add inherent methods
//! to it — an extension trait is the idiomatic way to hang `get_table` /
//! `get_str` / `get_bool` / `get_int_range` off it as if they were its own.
//! Keeps `lib.rs` clean — no shelby match arms leak into the top-level
//! parsing code, just `table.get_str("shell")`-style calls.

use shelby::{Table, Value};

use crate::ConfigError;

/// Typed, validating reads off a [`Table`], as methods instead of free
/// functions that all happened to take `&Table` as their first argument.
pub trait TableExt<'t> {
    /// Get a sub-table by key, or `None` if missing / not a table.
    fn get_table(&'t self, key: &str) -> Option<&'t Table<'t>>;

    /// Get a string value by key, or `None` if missing / not a string.
    fn get_str(&'t self, key: &str) -> Option<&'t str>;

    /// Get a boolean value by key, or `None` if missing / not a boolean.
    fn get_bool(&self, key: &str) -> Option<bool>;

    /// Read an integer in `[lo, hi]`, returning a typed validation error
    /// otherwise. `key` is the table-relative lookup key; `field` is the
    /// qualified name used in error messages (e.g. `"font.size"`). Returns
    /// `Ok(None)` when absent.
    fn get_int_range(
        &self,
        key: &'static str,
        field: &'static str,
        lo: i64,
        hi: i64,
        reason: &'static str,
    ) -> Result<Option<i64>, ConfigError>;
}

impl<'t> TableExt<'t> for Table<'t> {
    fn get_table(&'t self, key: &str) -> Option<&'t Self> {
        match self.get(key)? {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    fn get_str(&'t self, key: &str) -> Option<&'t str> {
        match self.get(key)? {
            Value::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    fn get_int_range(
        &self,
        key: &'static str,
        field: &'static str,
        lo: i64,
        hi: i64,
        reason: &'static str,
    ) -> Result<Option<i64>, ConfigError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Integer(n)) if (lo..=hi).contains(n) => Ok(Some(*n)),
            Some(Value::Integer(_)) => Err(ConfigError::InvalidValue { field, reason }),
            Some(_) => Err(ConfigError::InvalidValue { field, reason: "expected integer" }),
        }
    }
}
