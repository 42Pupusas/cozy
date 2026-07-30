//! `org.freedesktop.impl.portal.Settings.SettingChanged`'s signal body:
//! `(namespace: s, key: s, value: v)`.
//!
//! A stateless builder in the same spirit as `dbus_server::properties::
//! PropertiesChangedSignal` -- turning `(namespace, key, value)` into a
//! wire-ready `Vec<Value>` carries no state of its own between calls, so
//! it earns a marker type rather than a bare free function, matching this
//! workspace's rule that logic without an obvious owner gets a struct
//! instead of scattering as a loose `fn`.

use dbus::value::Value;

/// Builds `SettingChanged` signal bodies.
pub struct SettingChangedSignal;

impl SettingChangedSignal {
    /// The three-value signal body: `namespace`, `key`, and `value`
    /// wrapped in a `Variant` (matching the spec's `v` argument type, the
    /// same as `PropertiesChanged`'s changed-properties entries).
    #[must_use]
    pub fn body(namespace: &str, key: &str, value: Value) -> Vec<Value> {
        vec![
            Value::Str(namespace.to_string()),
            Value::Str(key.to_string()),
            Value::Variant(Box::new(value)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_wraps_the_value_in_a_variant() {
        let body =
            SettingChangedSignal::body("org.freedesktop.appearance", "color-scheme", Value::U32(1));
        assert_eq!(
            body,
            vec![
                Value::Str("org.freedesktop.appearance".into()),
                Value::Str("color-scheme".into()),
                Value::Variant(Box::new(Value::U32(1))),
            ]
        );
    }

    #[test]
    fn body_signature_matches_the_spec_shape() {
        let body =
            SettingChangedSignal::body("org.freedesktop.appearance", "color-scheme", Value::U32(1));
        let signature: String = body.iter().map(Value::signature).collect();
        assert_eq!(signature, "ssv");
    }
}
