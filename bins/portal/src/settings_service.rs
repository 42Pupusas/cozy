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
use portal_core::AppearanceSettings;

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

    /// A **scratch** well-known name, deliberately distinct from the real
    /// `org.freedesktop.impl.portal.desktop.mera` a future `.portal` file
    /// would advertise. `docs/portal-scoping.md`'s compatibility/rollback
    /// contract (section 4) requires developing every interface first on
    /// a scratch name, exactly like `crates/dbus-server`'s own scratch
    /// service integration test does -- only once this is validated
    /// against a real portal client does step 5 finish by shipping a
    /// `.portal` file naming the real activation name and routing it in
    /// `portals.conf`.
    pub const NAME: &'static str = "org.freedesktop.impl.portal.desktop.mera.scratch";

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
    /// method handlers, each closing over one `AppearanceSettings`
    /// snapshot taken from `cfg` at startup.
    ///
    /// Settings has no live-reload wiring yet -- unlike `bins/terminal`'s
    /// `ConfigWatcher`, a config edit made after this process starts
    /// won't be reflected (and no `SettingChanged` signal is ever
    /// emitted) until the daemon restarts. The interface descriptor still
    /// declares the signal for introspection's sake, since a real
    /// `impl.portal.Settings` always does; wiring an actual
    /// `inotify`-driven `SettingChanged` emission is a follow-up, not a
    /// blocker for this step's "does the real frontend path work" goal.
    #[must_use]
    pub fn dispatcher(cfg: &config::Config) -> Dispatcher {
        let settings = AppearanceSettings::from_config(cfg);

        let mut dispatcher = Dispatcher::new();
        dispatcher.register_object(Self::PATH, [Self::IFACE]);
        dispatcher.register_interface(Self::interface_descriptor());
        dispatcher.set_property(
            Self::PATH,
            Self::IFACE,
            "version",
            Value::U32(Self::VERSION),
        );

        dispatcher.register_method(
            Self::IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            move |_p, args| {
                let namespaces = Self::decode_namespaces(args)?;
                Ok(vec![settings.read_all(&namespaces)])
            },
        );
        dispatcher.register_method(Self::IFACE, "Read", "ss", "v", move |_p, args| {
            let (namespace, key) = Self::decode_read_args(args)?;
            settings
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

    #[test]
    fn dispatcher_answers_introspect_naming_the_settings_interface() {
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
        let dispatcher = SettingsService::dispatcher(&config::Config::default());
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
