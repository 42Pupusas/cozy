//! An extension trait for typed reads off a shelby [`Table`].
//!
//! `Table` is defined in an external crate, so we can't add inherent methods
//! to it — an extension trait is the idiomatic way to hang `get_table` /
//! `get_str` / `get_bool` / `get_int_range` off it as if they were its own.
//! Keeps `lib.rs` clean — no shelby match arms leak into the top-level
//! parsing code, just `table.get_str("shell")`-style calls.

use shelby::{Table, Value};

use crate::{Color, ConfigError};

/// Typed, validating reads off a [`Table`], as methods instead of free
/// functions that all happened to take `&Table` as their first argument.
pub trait TableExt<'t> {
    /// Get a sub-table by key, or `None` if missing / not a table.
    fn get_table(&'t self, key: &str) -> Option<&'t Table<'t>>;

    /// Get an array-of-tables by key (TOML `[[key]]` entries), or `None` if
    /// missing / not an array / not every element a table. Used for
    /// repeatable `[bar]` sub-sections like `[[bar.notifications]]`, where
    /// `shelby` already folds each `[[...]]` header into a `Value::Array`
    /// of `Value::Table` under the array's key -- this just narrows that
    /// down to `&[Table]` for callers that don't want to match on `Value`
    /// themselves.
    fn get_array_of_tables(&'t self, key: &str) -> Option<Vec<&'t Table<'t>>>;

    /// Get a string value by key, or `None` if missing / not a string.
    fn get_str(&'t self, key: &str) -> Option<&'t str>;

    /// Get a boolean value by key, or `None` if missing / not a boolean.
    fn get_bool(&self, key: &str) -> Option<bool>;

    /// Read an `#rrggbb` color, returning a typed validation error if the
    /// key is present but unparseable. `field` is the qualified name used
    /// in error messages (e.g. `"files.dir"`). Returns `Ok(None)` when
    /// absent.
    ///
    /// Exists because the alternative -- the six-line `get_str` +
    /// `parse_hex` + `ok_or(InvalidValue)` dance -- was already written out
    /// about twenty times across the `[bar]`, `[launcher]` and `[monitor]`
    /// tables, and `[files]` would have added fifteen more. Each copy
    /// restates the error field name by hand, which is exactly the sort of
    /// thing that gets copy-pasted and left pointing at the wrong key.
    fn get_color(&'t self, key: &str, field: &'static str) -> Result<Option<Color>, ConfigError>;

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

    /// Read a float in `[lo, hi]`, returning a typed validation error
    /// otherwise. Integers are accepted too (TOML's `1` is a valid `1.0`).
    /// `key` is the table-relative lookup key; `field` is the qualified name
    /// used in error messages (e.g. `"window.opacity"`). Returns `Ok(None)`
    /// when absent.
    fn get_float_range(
        &self,
        key: &'static str,
        field: &'static str,
        lo: f64,
        hi: f64,
        reason: &'static str,
    ) -> Result<Option<f64>, ConfigError>;
}

impl<'t> TableExt<'t> for Table<'t> {
    fn get_table(&'t self, key: &str) -> Option<&'t Self> {
        match self.get(key)? {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    fn get_array_of_tables(&'t self, key: &str) -> Option<Vec<&'t Self>> {
        match self.get(key)? {
            Value::Array(items) => items
                .iter()
                .map(|v| match v {
                    Value::Table(t) => Some(t),
                    _ => None,
                })
                .collect(),
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

    fn get_color(&'t self, key: &str, field: &'static str) -> Result<Option<Color>, ConfigError> {
        // A key that is present but not a string is a mistake worth
        // reporting, not something to silently ignore -- `dir = 0x89b4fa`
        // (a bare integer) is a plausible typo that would otherwise leave
        // the default in place with no explanation.
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Color::parse_hex(s.as_ref()).map_or(
                Err(ConfigError::InvalidValue {
                    field,
                    reason: "invalid #rrggbb hex color",
                }),
                |c| Ok(Some(c)),
            ),
            Some(_) => Err(ConfigError::InvalidValue {
                field,
                reason: "expected an #rrggbb hex color string",
            }),
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
            Some(_) => Err(ConfigError::InvalidValue {
                field,
                reason: "expected integer",
            }),
        }
    }

    // Every caller today validates a config field bounded to a tiny range
    // (e.g. window.opacity in 0.0..=1.0), so the i64->f64 conversion here
    // never approaches the 2^53 exact-integer boundary where precision loss
    // could occur -- it just needs to accept a bare TOML integer as a float.
    #[allow(clippy::cast_precision_loss)]
    fn get_float_range(
        &self,
        key: &'static str,
        field: &'static str,
        lo: f64,
        hi: f64,
        reason: &'static str,
    ) -> Result<Option<f64>, ConfigError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Float(n)) if (lo..=hi).contains(n) => Ok(Some(*n)),
            Some(Value::Integer(n)) if (lo..=hi).contains(&(*n as f64)) => Ok(Some(*n as f64)),
            Some(Value::Float(_) | Value::Integer(_)) => {
                Err(ConfigError::InvalidValue { field, reason })
            }
            Some(_) => Err(ConfigError::InvalidValue {
                field,
                reason: "expected a number",
            }),
        }
    }
}
