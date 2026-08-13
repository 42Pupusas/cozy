//! Wiring `portal_core::AppearanceSettings` to a real `dbus_server::Dispatcher`.
//!
//! The one place this binary translates generic wire arguments into the
//! pure domain calls `portal-core` already proves correct in isolation.
//!
//! Kept as its own module (not inlined in `main.rs`) because "how do I
//! answer `Read`/`ReadAll` over the wire" is a cohesive, independently
//! testable unit of its own -- distinct from `main`'s job of connecting,
//! claiming a name, and running the loop.

use dbus::value::Value;
use dbus_server::{
    Argument, Dispatcher, InterfaceDescriptor, MethodDescriptor, PropertyAccess,
    PropertyDescriptor, SignalDescriptor,
};

use crate::settings_state::SettingsState;

/// Groups this interface's `Dispatcher` wiring.
///
/// No state of its own (the actual settings snapshot is captured by value
/// into each registered closure), matching this workspace's rule that a
/// cohesive block of logic gets a named owner rather than free functions.
pub struct SettingsService;

impl SettingsService {
    /// The standard object path every `org.freedesktop.impl.portal.*`
    /// interface is served from.
    pub const PATH: &'static str = "/org/freedesktop/portal/desktop";

    /// `org.freedesktop.impl.portal.Settings`'s interface name.
    pub const IFACE: &'static str = "org.freedesktop.impl.portal.Settings";

    /// This backend's real D-Bus activation name, matching
    /// `bins/portal/data/cozy.portal`'s `DBusName` exactly -- the two
    /// must agree, or `xdg-desktop-portal` activates this process
    /// expecting it to claim a name it never does, and every call times
    /// out. `docs/portal-scoping.md`'s compatibility/rollback contract
    /// (section 4) was satisfied by developing on a scratch name first
    /// (`crates/dbus-server`'s own scratch-service test, and
    /// `tests/real_bus_probe.rs`'s `testprobe` name, both still do); this
    /// is the real name now that step 5's frontend path is proven.
    pub const NAME: &'static str = "org.freedesktop.impl.portal.desktop.cozy";

    /// This interface's reported version. `2` matches the shape this
    /// backend actually answers (`ReadAll`/`Read` over all four
    /// documented `org.freedesktop.appearance` keys, including the
    /// `contrast`/`reduced-motion` pair the spec added after version 1);
    /// see `crates/portal-core`'s own doc comment for why those two report
    /// a constant "no preference" rather than a real source.
    const VERSION: u32 = 2;

    /// Build a `Dispatcher` with the Settings object fully registered:
    /// its interface descriptor (for `Introspect`), its `version`
    /// property (for `Properties.Get`/`GetAll`), and its `ReadAll`/`Read`
    /// method handlers, each closing over `state` (not a one-shot
    /// snapshot) so a config reload -- applied by replacing `state`'s
    /// contents via [`SettingsState::reload`] -- is reflected in every
    /// call made after that point, not just ones made before this
    /// process started.
    #[must_use]
    pub fn dispatcher(state: SettingsState) -> Dispatcher {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_object(Self::PATH, [Self::IFACE]);
        dispatcher.register_interface(Self::interface_descriptor());
        dispatcher.set_property(
            Self::PATH,
            Self::IFACE,
            "version",
            Value::U32(Self::VERSION),
        );

        let read_all_state = state.clone();
        dispatcher.register_method(
            Self::IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            move |_p, args| {
                let namespaces = Self::decode_namespaces(args)?;
                Ok(vec![read_all_state.snapshot().read_all(&namespaces)])
            },
        );
        dispatcher.register_method(Self::IFACE, "Read", "ss", "v", move |_p, args| {
            let (namespace, key) = Self::decode_read_args(args)?;
            state
                .snapshot()
                .read(&namespace, &key)
                .map(|value| vec![Value::Variant(Box::new(value))])
                .ok_or_else(|| format!("No such key '{key}' in namespace '{namespace}'"))
        });

        dispatcher
    }

    /// This interface's full shape for `Introspect`: both methods (with
    /// argument names/directions matching the upstream
    /// `org.freedesktop.impl.portal.Settings.xml` reference), the
    /// `SettingChanged` signal, and the `version` property.
    fn interface_descriptor() -> InterfaceDescriptor {
        InterfaceDescriptor::new(Self::IFACE)
            .method(MethodDescriptor::new(
                "ReadAll",
                vec![Argument::new("namespaces", "as")],
                vec![Argument::new("value", "a{sa{sv}}")],
            ))
            .method(MethodDescriptor::new(
                "Read",
                vec![Argument::new("namespace", "s"), Argument::new("key", "s")],
                vec![Argument::new("value", "v")],
            ))
            .signal(SignalDescriptor::new(
                "SettingChanged",
                vec![
                    Argument::new("namespace", "s"),
                    Argument::new("key", "s"),
                    Argument::new("value", "v"),
                ],
            ))
            .property(PropertyDescriptor::new(
                "version",
                "u",
                PropertyAccess::Read,
            ))
    }

    /// `ReadAll`'s single `as` argument, decoded to owned strings. The
    /// router already checked the call's signature is `"as"` before this
    /// handler ever runs, so a malformed element (anything not
    /// string-like) means the *decoder*, not the caller, has a bug --
    /// still reported as a normal `Failed` dispatch rather than a panic,
    /// since nothing here can prove that invariant at the type level.
    fn decode_namespaces(args: &[Value]) -> Result<Vec<String>, String> {
        let items = args
            .first()
            .and_then(Value::as_items)
            .ok_or_else(|| "ReadAll expects one array argument".to_string())?;
        items
            .iter()
            .map(|item| {
                item.as_display_string()
                    .ok_or_else(|| "ReadAll's namespaces array must contain strings".to_string())
            })
            .collect()
    }

    /// `Read`'s two `s` arguments, decoded to owned strings.
    fn decode_read_args(args: &[Value]) -> Result<(String, String), String> {
        let namespace = args
            .first()
            .and_then(Value::as_display_string)
            .ok_or_else(|| "Read expects (namespace: s, key: s)".to_string())?;
        let key = args
            .get(1)
            .and_then(Value::as_display_string)
            .ok_or_else(|| "Read expects (namespace: s, key: s)".to_string())?;
        Ok((namespace, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMESPACE: &str = "org.freedesktop.appearance";

    fn state() -> SettingsState {
        SettingsState::new(&config::Config::default())
    }

    #[test]
    fn dispatcher_answers_introspect_naming_the_settings_interface() {
        let dispatcher = SettingsService::dispatcher(state());
        let outcome = dispatcher
            .dispatch(
                SettingsService::PATH,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                "",
                &[],
            )
            .unwrap();
        let Value::Str(xml) = &outcome.values[0] else {
            panic!("Introspect must reply with a single string");
        };
        assert!(xml.contains(&format!("<interface name=\"{}\">", SettingsService::IFACE)));
        assert!(xml.contains("<method name=\"ReadAll\">"));
        assert!(xml.contains("<method name=\"Read\">"));
        assert!(xml.contains("<signal name=\"SettingChanged\">"));
    }

    #[test]
    fn dispatcher_answers_properties_get_all_with_the_version_property() {
        let dispatcher = SettingsService::dispatcher(state());
        let outcome = dispatcher
            .dispatch(
                SettingsService::PATH,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                "s",
                &[Value::Str(SettingsService::IFACE.into())],
            )
            .unwrap();
        assert_eq!(
            outcome.values,
            vec![Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("version".into())),
                Box::new(Value::Variant(Box::new(Value::U32(2)))),
            )])]
        );
    }

    #[test]
    fn read_all_with_no_filter_reaches_the_real_appearance_settings() {
        let dispatcher = SettingsService::dispatcher(state());
        let outcome = dispatcher
            .dispatch(
                SettingsService::PATH,
                SettingsService::IFACE,
                "ReadAll",
                "as",
                &[Value::Array(vec![])],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "a{sa{sv}}");
        let Value::Array(entries) = &outcome.values[0] else {
            panic!("expected the outer a{{sa{{sv}}}} array");
        };
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn read_all_filtered_to_a_non_matching_namespace_is_empty() {
        let dispatcher = SettingsService::dispatcher(state());
        let outcome = dispatcher
            .dispatch(
                SettingsService::PATH,
                SettingsService::IFACE,
                "ReadAll",
                "as",
                &[Value::Array(vec![Value::Str("org.example.other".into())])],
            )
            .unwrap();
        assert_eq!(outcome.values, vec![Value::Array(vec![])]);
    }

    #[test]
    fn read_reports_color_scheme_wrapped_in_a_variant() {
        let dispatcher = SettingsService::dispatcher(state());
        let outcome = dispatcher
            .dispatch(
                SettingsService::PATH,
                SettingsService::IFACE,
                "Read",
                "ss",
                &[
                    Value::Str(NAMESPACE.into()),
                    Value::Str("color-scheme".into()),
                ],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "v");
        assert!(matches!(outcome.values[0], Value::Variant(_)));
    }

    #[test]
    fn read_on_an_unknown_key_is_a_failed_dispatch() {
        let dispatcher = SettingsService::dispatcher(state());
        let err = dispatcher
            .dispatch(
                SettingsService::PATH,
                SettingsService::IFACE,
                "Read",
                "ss",
                &[
                    Value::Str(NAMESPACE.into()),
                    Value::Str("does-not-exist".into()),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, dbus_server::DispatchError::Failed(_)));
    }

    #[test]
    fn read_on_the_wrong_namespace_is_a_failed_dispatch() {
        let dispatcher = SettingsService::dispatcher(state());
        let err = dispatcher
            .dispatch(
                SettingsService::PATH,
                SettingsService::IFACE,
                "Read",
                "ss",
                &[
                    Value::Str("org.example.other".into()),
                    Value::Str("color-scheme".into()),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, dbus_server::DispatchError::Failed(_)));
    }
}
