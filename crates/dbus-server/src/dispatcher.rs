//! The single decision a server's receive loop actually needs.
//!
//! Given one incoming method call, produce either a reply body or a
//! [`DispatchError`] -- folding together everything this crate's other
//! pieces track separately.
//!
//! [`ObjectRegistry`] (what exists), [`MethodCallRouter`] (what handles
//! it), [`PropertyTable`] (what its properties currently are), and
//! [`InterfaceDescriptor`] (what its introspection XML should say) each
//! stay independently testable in their own module. [`Dispatcher`] is the
//! one place that has to know about all four at once, because two
//! interfaces every object answers --
//! `org.freedesktop.DBus.Introspectable.Introspect` and
//! `org.freedesktop.DBus.Properties.Get`/`GetAll` -- have a single,
//! correct, generic implementation in terms of that combined state, and
//! a real service (a portal daemon, the scratch service that proves this
//! crate out) shouldn't have to hand-wire those two interfaces for every
//! object it ever registers the way [`MethodCallRouter::register`]
//! requires for its own business methods.
//!
//! `Properties.Set` is deliberately *not* handled generically here: unlike
//! `Get`/`GetAll`, which only ever read [`PropertyTable`]'s state, setting
//! a property is an application-level decision (is this property
//! read-only? does the new value need validating?) that belongs in a
//! caller-registered [`MethodCallRouter`] handler, not in this crate's
//! generic dispatch. Calling `Properties.Set` on an object that hasn't
//! registered it falls through to the router and reports
//! [`DispatchError::UnknownMethod`], the same as any other
//! never-registered method.

use dbus::value::Value;

use crate::introspection::{InterfaceDescriptor, NodeIntrospection};
use crate::properties::PropertyTable;
use crate::registry::ObjectRegistry;
use crate::router::{DispatchError, DispatchOutcome, MethodCallRouter};

const INTROSPECTABLE_IFACE: &str = "org.freedesktop.DBus.Introspectable";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// Ties together the pieces a real dispatch needs.
///
/// Combines an [`ObjectRegistry`], a [`MethodCallRouter`], a
/// [`PropertyTable`], and an interface-name -> [`InterfaceDescriptor`]
/// catalog (used only to render introspection XML; a path's actual
/// implemented interfaces still come from the registry) into the one
/// `dispatch` call a server's receive loop needs per incoming message.
#[derive(Default)]
pub struct Dispatcher {
    registry: ObjectRegistry,
    router: MethodCallRouter,
    properties: PropertyTable,
    /// Interface name -> its full shape, so `Introspect` can render every
    /// interface a path claims (via the registry) without the caller
    /// having to also hand a `NodeIntrospection` to every
    /// `register_object` call and keep the two in sync by hand.
    interfaces: std::collections::HashMap<String, InterfaceDescriptor>,
}

impl Dispatcher {
    /// A dispatcher with nothing registered yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish an object at `path` implementing `interfaces`. See
    /// [`ObjectRegistry::register`] -- this is a direct pass-through.
    pub fn register_object<P, I, S>(&mut self, path: P, interfaces: I)
    where
        P: Into<String>,
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.registry.register(path, interfaces);
    }

    /// Remove `path`. See [`ObjectRegistry::unregister`].
    pub fn unregister_object(&mut self, path: &str) -> bool {
        self.registry.unregister(path)
    }

    /// Register `descriptor`'s full shape (methods, signals, properties)
    /// under its own interface name, so `Introspect` can describe it on
    /// every object that claims to implement it. This is independent of
    /// [`Self::register_method`]: the descriptor documents shape for
    /// introspection, the router entry supplies the actual handler --
    /// keeping them in agreement is the caller's responsibility, same as
    /// any real D-Bus service authoring both an XML interface file and
    /// its method table by hand.
    pub fn register_interface(&mut self, descriptor: InterfaceDescriptor) {
        self.interfaces
            .insert(descriptor.name().to_string(), descriptor);
    }

    /// Register a method handler. See [`MethodCallRouter::register`].
    pub fn register_method<F>(
        &mut self,
        interface: impl Into<String>,
        member: impl Into<String>,
        signature: impl Into<String>,
        reply_signature: impl Into<String>,
        handler: F,
    ) where
        F: Fn(&str, &[Value]) -> Result<Vec<Value>, String> + Send + Sync + 'static,
    {
        self.router
            .register(interface, member, signature, reply_signature, handler);
    }

    /// Track `(path, interface)` with zero properties. See
    /// [`PropertyTable::ensure`].
    pub fn ensure_properties(&mut self, path: impl Into<String>, interface: impl Into<String>) {
        self.properties.ensure(path, interface);
    }

    /// Set a property's current value. See [`PropertyTable::set`].
    pub fn set_property(
        &mut self,
        path: impl Into<String>,
        interface: impl Into<String>,
        property: impl Into<String>,
        value: Value,
    ) -> Option<Value> {
        self.properties.set(path, interface, property, value)
    }

    /// Dispatch one incoming method call, handling `Introspect` and
    /// `Properties.Get`/`GetAll` generically before falling through to
    /// the registered method table for everything else (including
    /// `Properties.Set`, which is never handled generically -- see the
    /// module doc comment).
    pub fn dispatch(
        &self,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        args: &[Value],
    ) -> Result<DispatchOutcome, DispatchError> {
        if !self.registry.contains(path) {
            return Err(DispatchError::UnknownObject { path: path.into() });
        }
        if interface == INTROSPECTABLE_IFACE && member == "Introspect" {
            return self.dispatch_introspect(path, signature);
        }
        if interface == PROPERTIES_IFACE {
            match member {
                "Get" => return self.dispatch_properties_get(path, signature, args),
                "GetAll" => return self.dispatch_properties_get_all(path, signature, args),
                _ => {} // e.g. "Set" -- falls through to the router below.
            }
        }
        self.router
            .dispatch(&self.registry, path, interface, member, signature, args)
    }

    fn dispatch_introspect(
        &self,
        path: &str,
        signature: &str,
    ) -> Result<DispatchOutcome, DispatchError> {
        Self::require_signature(INTROSPECTABLE_IFACE, "Introspect", signature, "")?;
        // `contains` was already checked by `dispatch`; an object with no
        // interfaces beyond the two standard ones is still valid (a bare
        // Request/Session object, say), so `unwrap_or` to an empty slice
        // rather than treating that as an error.
        let mut node = NodeIntrospection::new();
        for interface_name in self.registry.interfaces_of(path).unwrap_or(&[]) {
            if let Some(descriptor) = self.interfaces.get(interface_name) {
                node = node.interface(descriptor.clone());
            }
        }
        Ok(DispatchOutcome {
            reply_signature: "s".to_string(),
            values: vec![Value::Str(node.to_xml())],
        })
    }

    fn dispatch_properties_get(
        &self,
        path: &str,
        signature: &str,
        args: &[Value],
    ) -> Result<DispatchOutcome, DispatchError> {
        Self::require_signature(PROPERTIES_IFACE, "Get", signature, "ss")?;
        let interface_name = Self::string_arg(PROPERTIES_IFACE, "Get", args, 0)?;
        let property_name = Self::string_arg(PROPERTIES_IFACE, "Get", args, 1)?;
        let value = self
            .properties
            .get(path, &interface_name, &property_name)
            .ok_or_else(|| {
                DispatchError::Failed(format!(
                    "No such property '{property_name}' on interface '{interface_name}'"
                ))
            })?;
        Ok(DispatchOutcome {
            reply_signature: "v".to_string(),
            values: vec![Value::Variant(Box::new(value.clone()))],
        })
    }

    fn dispatch_properties_get_all(
        &self,
        path: &str,
        signature: &str,
        args: &[Value],
    ) -> Result<DispatchOutcome, DispatchError> {
        Self::require_signature(PROPERTIES_IFACE, "GetAll", signature, "s")?;
        let interface_name = Self::string_arg(PROPERTIES_IFACE, "GetAll", args, 0)?;
        let all = self
            .properties
            .get_all(path, &interface_name)
            .ok_or_else(|| {
                DispatchError::Failed(format!(
                    "Object does not implement interface '{interface_name}'"
                ))
            })?;
        Ok(DispatchOutcome {
            reply_signature: "a{sv}".to_string(),
            values: vec![all],
        })
    }

    /// Report [`DispatchError::InvalidArgs`] if `got` doesn't match
    /// `expected`, matching the same structural check
    /// [`MethodCallRouter::dispatch`] does for a registered method --
    /// these two generically-handled interfaces need it spelled out by
    /// hand since they never go through the router's own method table.
    fn require_signature(
        interface: &str,
        member: &str,
        got: &str,
        expected: &str,
    ) -> Result<(), DispatchError> {
        if got == expected {
            Ok(())
        } else {
            Err(DispatchError::InvalidArgs {
                interface: interface.into(),
                member: member.into(),
                expected: expected.into(),
                got: got.into(),
            })
        }
    }

    /// Pull the `index`th argument as a string, reporting the same
    /// `InvalidArgs` a caller would get for any other malformed call --
    /// this only fires if `args` doesn't actually match the signature
    /// `require_signature` already checked, which would mean the caller
    /// (whatever decoded the wire message into `args`) has a bug, not the
    /// far end.
    fn string_arg(
        interface: &str,
        member: &str,
        args: &[Value],
        index: usize,
    ) -> Result<String, DispatchError> {
        args.get(index)
            .and_then(Value::as_display_string)
            .ok_or_else(|| DispatchError::InvalidArgs {
                interface: interface.into(),
                member: member.into(),
                expected: "s".into(),
                got: "?".into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::introspection::{Argument, MethodDescriptor};

    const SETTINGS_IFACE: &str = "org.freedesktop.impl.portal.Settings";
    const SETTINGS_PATH: &str = "/org/freedesktop/portal/desktop";

    fn dispatcher_with_settings_object() -> Dispatcher {
        let mut dispatcher = Dispatcher::new();
        dispatcher.register_object(SETTINGS_PATH, [SETTINGS_IFACE]);
        dispatcher.register_interface(InterfaceDescriptor::new(SETTINGS_IFACE).method(
            MethodDescriptor::new(
                "ReadAll",
                vec![Argument::new("namespaces", "as")],
                vec![Argument::new("value", "a{sa{sv}}")],
            ),
        ));
        dispatcher.register_method(SETTINGS_IFACE, "ReadAll", "as", "a{sa{sv}}", |_p, _a| {
            Ok(vec![Value::Array(vec![])])
        });
        dispatcher.set_property(SETTINGS_PATH, SETTINGS_IFACE, "version", Value::U32(2));
        dispatcher
    }

    #[test]
    fn dispatch_to_an_unregistered_path_is_unknown_object() {
        let dispatcher = Dispatcher::new();
        let err = dispatcher
            .dispatch(SETTINGS_PATH, INTROSPECTABLE_IFACE, "Introspect", "", &[])
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownObject {
                path: SETTINGS_PATH.into()
            }
        );
    }

    #[test]
    fn introspect_renders_every_registered_interface_on_the_object() {
        let dispatcher = dispatcher_with_settings_object();
        let outcome = dispatcher
            .dispatch(SETTINGS_PATH, INTROSPECTABLE_IFACE, "Introspect", "", &[])
            .unwrap();
        assert_eq!(outcome.reply_signature, "s");
        let Value::Str(xml) = &outcome.values[0] else {
            panic!("Introspect must reply with a single string");
        };
        assert!(xml.contains(&format!("<interface name=\"{SETTINGS_IFACE}\">")));
        assert!(xml.contains("<method name=\"ReadAll\">"));
        // The two standard interfaces are always present too.
        assert!(xml.contains("<interface name=\"org.freedesktop.DBus.Introspectable\">"));
    }

    #[test]
    fn introspect_with_a_nonempty_signature_is_invalid_args() {
        let dispatcher = dispatcher_with_settings_object();
        let err = dispatcher
            .dispatch(
                SETTINGS_PATH,
                INTROSPECTABLE_IFACE,
                "Introspect",
                "s",
                &[Value::Str("unexpected".into())],
            )
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::InvalidArgs {
                interface: INTROSPECTABLE_IFACE.into(),
                member: "Introspect".into(),
                expected: String::new(),
                got: "s".into(),
            }
        );
    }

    #[test]
    fn properties_get_returns_the_wrapped_value() {
        let dispatcher = dispatcher_with_settings_object();
        let outcome = dispatcher
            .dispatch(
                SETTINGS_PATH,
                PROPERTIES_IFACE,
                "Get",
                "ss",
                &[
                    Value::Str(SETTINGS_IFACE.into()),
                    Value::Str("version".into()),
                ],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "v");
        assert_eq!(
            outcome.values,
            vec![Value::Variant(Box::new(Value::U32(2)))]
        );
    }

    #[test]
    fn properties_get_on_a_missing_property_is_a_failed_dispatch() {
        let dispatcher = dispatcher_with_settings_object();
        let err = dispatcher
            .dispatch(
                SETTINGS_PATH,
                PROPERTIES_IFACE,
                "Get",
                "ss",
                &[
                    Value::Str(SETTINGS_IFACE.into()),
                    Value::Str("missing".into()),
                ],
            )
            .unwrap_err();
        assert!(matches!(err, DispatchError::Failed(_)));
    }

    #[test]
    fn properties_get_all_returns_every_property_on_the_interface() {
        let dispatcher = dispatcher_with_settings_object();
        let outcome = dispatcher
            .dispatch(
                SETTINGS_PATH,
                PROPERTIES_IFACE,
                "GetAll",
                "s",
                &[Value::Str(SETTINGS_IFACE.into())],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "a{sv}");
        assert_eq!(
            outcome.values,
            vec![Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("version".into())),
                Box::new(Value::Variant(Box::new(Value::U32(2)))),
            )])]
        );
    }

    #[test]
    fn properties_get_all_with_the_wrong_signature_is_invalid_args() {
        let dispatcher = dispatcher_with_settings_object();
        let err = dispatcher
            .dispatch(SETTINGS_PATH, PROPERTIES_IFACE, "GetAll", "", &[])
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::InvalidArgs {
                interface: PROPERTIES_IFACE.into(),
                member: "GetAll".into(),
                expected: "s".into(),
                got: String::new(),
            }
        );
    }

    #[test]
    fn properties_set_is_not_handled_generically_and_falls_through_to_unknown_method() {
        let dispatcher = dispatcher_with_settings_object();
        let err = dispatcher
            .dispatch(
                SETTINGS_PATH,
                PROPERTIES_IFACE,
                "Set",
                "ssv",
                &[
                    Value::Str(SETTINGS_IFACE.into()),
                    Value::Str("version".into()),
                    Value::Variant(Box::new(Value::U32(3))),
                ],
            )
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownMethod {
                interface: PROPERTIES_IFACE.into(),
                member: "Set".into(),
            }
        );
    }

    #[test]
    fn business_methods_still_dispatch_normally() {
        let dispatcher = dispatcher_with_settings_object();
        let outcome = dispatcher
            .dispatch(
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[Value::Array(vec![])],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "a{sa{sv}}");
    }

    #[test]
    fn unregistering_an_object_makes_every_call_against_it_unknown_object_again() {
        let mut dispatcher = dispatcher_with_settings_object();
        assert!(dispatcher.unregister_object(SETTINGS_PATH));
        let err = dispatcher
            .dispatch(SETTINGS_PATH, INTROSPECTABLE_IFACE, "Introspect", "", &[])
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownObject {
                path: SETTINGS_PATH.into()
            }
        );
    }
}
