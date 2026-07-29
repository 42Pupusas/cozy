//! `org.freedesktop.DBus.Properties`: the live property values a service
//! exposes per object/interface, and the `a{sv}` / `PropertiesChanged`
//! wire shapes built from them.
//!
//! Two independent pieces, on purpose:
//!
//! - [`PropertyTable`] is the *state*: current property values, keyed by
//!   object path and interface (an object can implement several
//!   interfaces, each with its own property namespace -- `Settings` and a
//!   hypothetical vendor extension interface on the same path don't share
//!   one flat map). It only knows values, not method dispatch; wiring its
//!   `get`/`get_all`/`set` up as the actual handlers a
//!   [`crate::router::MethodCallRouter`] calls for
//!   `Get`/`GetAll`/`Set` is a later slice's job (bins/portal's own
//!   wiring, since only it knows which interfaces are read-only, e.g.
//!   `Settings.version`, versus read-write).
//! - [`PropertiesChangedSignal`] is a pure, stateless builder for the
//!   signal body -- a zero-sized marker type in the same spirit as
//!   [`dbus::message::MessageBuilder`] and [`dbus::marshal::Marshaller`]:
//!   turning `(interface, changed, invalidated)` into wire-ready `Value`s
//!   carries no state of its own between calls.

use dbus::value::Value;
use std::collections::{BTreeMap, HashMap};

/// Live property values for every (object path, interface) this service
/// currently tracks.
///
/// Properties within one interface are kept in a [`BTreeMap`] rather than
/// a [`HashMap`] so [`PropertyTable::get_all`] can hand back a
/// deterministically ordered `a{sv}` -- useful for tests and for anyone
/// diffing two `GetAll` replies, and it costs nothing here since these
/// maps are small (a handful of properties per interface at most).
#[derive(Default)]
pub struct PropertyTable {
    interfaces: HashMap<(String, String), BTreeMap<String, Value>>,
}

impl PropertyTable {
    /// An empty table: no object/interface has been touched yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensure `(path, interface)` is tracked, even with zero properties.
    ///
    /// Distinguishes "this interface exists here but happens to have no
    /// properties right now" (an empty `a{sv}` from `get_all`) from "this
    /// interface was never registered at all" (`None` from `get_all`) --
    /// the same distinction [`crate::registry::ObjectRegistry`] draws
    /// between a zero-interface object and an absent path. A caller that
    /// only ever calls [`Self::set`] never needs this, since `set`
    /// implicitly creates the entry; it exists for interfaces whose
    /// property set is genuinely empty today but should still answer
    /// `GetAll` with `{}` rather than `UnknownInterface`.
    pub fn ensure(&mut self, path: impl Into<String>, interface: impl Into<String>) {
        self.interfaces
            .entry((path.into(), interface.into()))
            .or_default();
    }

    /// Set `property`'s current value on `(path, interface)`, creating the
    /// entry if this is the first property ever set there. Returns the
    /// previous value, if any -- callers building `PropertiesChanged`
    /// notifications typically only want to emit a signal when this
    /// differs from the new value.
    pub fn set(
        &mut self,
        path: impl Into<String>,
        interface: impl Into<String>,
        property: impl Into<String>,
        value: Value,
    ) -> Option<Value> {
        self.interfaces
            .entry((path.into(), interface.into()))
            .or_default()
            .insert(property.into(), value)
    }

    /// The current value of `property` on `(path, interface)`, or `None`
    /// if either the interface isn't tracked there or it has no such
    /// property.
    #[must_use]
    pub fn get(&self, path: &str, interface: &str, property: &str) -> Option<&Value> {
        self.interfaces
            .get(&(path.to_string(), interface.to_string()))?
            .get(property)
    }

    /// Every property on `(path, interface)` as a ready-to-marshal `a{sv}`
    /// value (an array of `(name, Variant(value))` dict entries, ordered
    /// by property name) -- or `None` if `(path, interface)` was never
    /// registered via [`Self::ensure`] or [`Self::set`].
    #[must_use]
    pub fn get_all(&self, path: &str, interface: &str) -> Option<Value> {
        let properties = self
            .interfaces
            .get(&(path.to_string(), interface.to_string()))?;
        let entries = properties
            .iter()
            .map(|(name, value)| {
                Value::DictEntry(
                    Box::new(Value::Str(name.clone())),
                    Box::new(Value::Variant(Box::new(value.clone()))),
                )
            })
            .collect();
        Some(Value::Array(entries))
    }

    /// Whether `(path, interface)` is tracked at all (with or without any
    /// properties set on it).
    #[must_use]
    pub fn contains(&self, path: &str, interface: &str) -> bool {
        self.interfaces
            .contains_key(&(path.to_string(), interface.to_string()))
    }

    /// How many `(path, interface)` pairs are tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.interfaces.len()
    }

    /// Whether nothing has been tracked at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.interfaces.is_empty()
    }
}

/// Builds `org.freedesktop.DBus.Properties.PropertiesChanged` signal
/// bodies: `(interface_name: s, changed_properties: a{sv},
/// invalidated_properties: as)`.
///
/// A zero-sized marker type -- see [`dbus::marshal::Marshaller`]'s doc
/// comment for why a stateless value-shaping operation like this gets a
/// named owner instead of a free function.
pub struct PropertiesChangedSignal;

impl PropertiesChangedSignal {
    /// Build the three-value signal body. `changed` is emitted as the
    /// `a{sv}` (each value wrapped in a `Variant`, matching the spec's
    /// requirement that changed properties carry their new value inline);
    /// `invalidated` lists properties whose new value the caller wants
    /// subscribers to re-fetch via `Get` instead of receiving inline --
    /// the same two channels real services (e.g. `NetworkManager`) use to
    /// avoid marshalling large properties into every notification.
    #[must_use]
    pub fn body(
        interface: &str,
        changed: &[(String, Value)],
        invalidated: &[String],
    ) -> Vec<Value> {
        let changed_entries = changed
            .iter()
            .map(|(name, value)| {
                Value::DictEntry(
                    Box::new(Value::Str(name.clone())),
                    Box::new(Value::Variant(Box::new(value.clone()))),
                )
            })
            .collect();
        let invalidated_entries = invalidated.iter().cloned().map(Value::Str).collect();
        vec![
            Value::Str(interface.to_string()),
            Value::Array(changed_entries),
            Value::Array(invalidated_entries),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS_IFACE: &str = "org.freedesktop.impl.portal.Settings";
    const SETTINGS_PATH: &str = "/org/freedesktop/portal/desktop";

    #[test]
    fn a_fresh_table_is_empty() {
        let table = PropertyTable::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
        assert!(!table.contains(SETTINGS_PATH, SETTINGS_IFACE));
    }

    #[test]
    fn setting_a_property_makes_it_visible_and_tracks_the_interface() {
        let mut table = PropertyTable::new();
        table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2));

        assert!(table.contains(SETTINGS_PATH, SETTINGS_IFACE));
        assert_eq!(
            table.get(SETTINGS_PATH, SETTINGS_IFACE, "version"),
            Some(&Value::U32(2))
        );
    }

    #[test]
    fn setting_a_property_again_returns_the_previous_value() {
        let mut table = PropertyTable::new();
        assert_eq!(
            table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(1)),
            None
        );
        assert_eq!(
            table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2)),
            Some(Value::U32(1))
        );
        assert_eq!(
            table.get(SETTINGS_PATH, SETTINGS_IFACE, "version"),
            Some(&Value::U32(2))
        );
    }

    #[test]
    fn get_on_an_untracked_interface_or_missing_property_is_none() {
        let mut table = PropertyTable::new();
        table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2));

        assert_eq!(
            table.get(SETTINGS_PATH, "org.example.Other", "version"),
            None
        );
        assert_eq!(table.get(SETTINGS_PATH, SETTINGS_IFACE, "missing"), None);
    }

    #[test]
    fn get_all_on_an_untracked_interface_is_none() {
        let table = PropertyTable::new();
        assert_eq!(table.get_all(SETTINGS_PATH, SETTINGS_IFACE), None);
    }

    #[test]
    fn ensure_tracks_an_interface_with_zero_properties_distinct_from_untracked() {
        let mut table = PropertyTable::new();
        table.ensure(SETTINGS_PATH, SETTINGS_IFACE);

        assert!(table.contains(SETTINGS_PATH, SETTINGS_IFACE));
        assert_eq!(
            table.get_all(SETTINGS_PATH, SETTINGS_IFACE),
            Some(Value::Array(vec![]))
        );
    }

    #[test]
    fn get_all_returns_every_property_as_a_sorted_sv_array() {
        let mut table = PropertyTable::new();
        table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2));
        table.set(
            SETTINGS_PATH,
            SETTINGS_IFACE,
            "accent-color",
            Value::Double(0.2),
        );

        let all = table.get_all(SETTINGS_PATH, SETTINGS_IFACE).unwrap();
        assert_eq!(
            all,
            Value::Array(vec![
                Value::DictEntry(
                    Box::new(Value::Str("accent-color".into())),
                    Box::new(Value::Variant(Box::new(Value::Double(0.2)))),
                ),
                Value::DictEntry(
                    Box::new(Value::Str("version".into())),
                    Box::new(Value::Variant(Box::new(Value::U32(2)))),
                ),
            ])
        );
    }

    #[test]
    fn distinct_interfaces_on_the_same_path_have_independent_property_sets() {
        let mut table = PropertyTable::new();
        table.set(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2));
        table.set(
            SETTINGS_PATH,
            "org.example.Other",
            "version",
            Value::U32(99),
        );

        assert_eq!(
            table.get(SETTINGS_PATH, SETTINGS_IFACE, "version"),
            Some(&Value::U32(2))
        );
        assert_eq!(
            table.get(SETTINGS_PATH, "org.example.Other", "version"),
            Some(&Value::U32(99))
        );
    }

    #[test]
    fn properties_changed_body_wraps_changed_values_as_variants() {
        let body = PropertiesChangedSignal::body(
            SETTINGS_IFACE,
            &[("scheme".to_string(), Value::U32(1))],
            &[],
        );
        assert_eq!(
            body,
            vec![
                Value::Str(SETTINGS_IFACE.into()),
                Value::Array(vec![Value::DictEntry(
                    Box::new(Value::Str("scheme".into())),
                    Box::new(Value::Variant(Box::new(Value::U32(1)))),
                )]),
                Value::Array(vec![]),
            ]
        );
    }

    #[test]
    fn properties_changed_body_lists_invalidated_names_as_plain_strings() {
        let body = PropertiesChangedSignal::body(SETTINGS_IFACE, &[], &["large-blob".to_string()]);
        assert_eq!(
            body,
            vec![
                Value::Str(SETTINGS_IFACE.into()),
                Value::Array(vec![]),
                Value::Array(vec![Value::Str("large-blob".into())]),
            ]
        );
    }

    #[test]
    fn properties_changed_body_signature_matches_the_spec_shape_when_populated() {
        // Value::signature can only infer an array's element type from an
        // actual element (an empty array degenerates to `av`, its
        // documented gap), so this exercises the real `a{sv}`/`as` shapes
        // with at least one entry in each array.
        let body = PropertiesChangedSignal::body(
            SETTINGS_IFACE,
            &[("scheme".to_string(), Value::U32(1))],
            &["large-blob".to_string()],
        );
        let signature: String = body.iter().map(Value::signature).collect();
        assert_eq!(signature, "sa{sv}as");
    }
}
