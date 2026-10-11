//! Signature-checked method dispatch.
//!
//! Given an incoming method call's `(path, interface, member, signature,
//! args)`, find the registered handler for it and invoke it -- or produce
//! the same standard D-Bus error a real service would, if any part of
//! that doesn't match.
//!
//! Deliberately layered on top of [`crate::registry::ObjectRegistry`]
//! rather than duplicating its bookkeeping: the registry answers "does
//! this object path exist, what interfaces does it implement", which is
//! object *lifecycle* knowledge (objects come and go, e.g. per-request
//! portal objects). The router answers "given an interface and member,
//! do we have a handler and does the caller's signature match", which is
//! static *method-table* knowledge fixed at startup. [`MethodCallRouter`]
//! takes an `&ObjectRegistry` as an argument to `dispatch` instead of
//! owning one, so the two stay independently testable and a caller who
//! only needs one of the two isn't forced to construct the other.
//!
//! This module intentionally stops at *deciding* the outcome (a body plus
//! its reply signature, or a [`DispatchError`]) rather than encoding a
//! wire reply -- turning a [`DispatchError`] into an actual `Error`-typed
//! message frame is a `dbus::message` concern for a later slice, once
//! that crate grows a `build_error` alongside its existing
//! `build_method_return`.

use dbus::value::Value;
use std::collections::HashMap;

use crate::call_context::CallContext;
use crate::registry::ObjectRegistry;

/// The two interfaces every registered object answers implicitly, per
/// [`ObjectRegistry`]'s own contract: it deliberately does not track
/// these in a path's interface list (see its doc comment), so
/// [`MethodCallRouter::dispatch`] must check for them here instead of
/// via [`ObjectRegistry::implements`] -- otherwise a real `Introspect`
/// or `Properties.Get`/`GetAll` call against any live object would
/// wrongly fail as [`DispatchError::UnknownInterface`].
const STANDARD_INTERFACES: [&str; 2] = [
    "org.freedesktop.DBus.Introspectable",
    "org.freedesktop.DBus.Properties",
];

/// The standard D-Bus error a failed dispatch corresponds to, carrying
/// enough detail for whoever builds the wire reply to fill in a useful
/// error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// `path` isn't registered at all.
    UnknownObject { path: String },
    /// `path` exists but doesn't claim to implement `interface`.
    UnknownInterface { path: String, interface: String },
    /// `interface` has no `member` registered on this router (regardless
    /// of whether any object implements `interface` -- the router's
    /// method table is global, not per-path).
    UnknownMethod { interface: String, member: String },
    /// A method matching `(interface, member)` exists, but the caller's
    /// signature doesn't match what it was registered with.
    InvalidArgs {
        interface: String,
        member: String,
        expected: String,
        got: String,
    },
    /// The handler itself rejected the call at the application level
    /// (e.g. a portal method validating one of its option-dict entries).
    /// Distinct from the four structural mismatches above, which the
    /// router itself detects before ever calling a handler.
    Failed(String),
}

impl DispatchError {
    /// The D-Bus error name a wire `Error` reply should carry for this
    /// outcome, e.g. `"org.freedesktop.DBus.Error.UnknownMethod"`.
    #[must_use]
    pub const fn error_name(&self) -> &'static str {
        match self {
            Self::UnknownObject { .. } => "org.freedesktop.DBus.Error.UnknownObject",
            Self::UnknownInterface { .. } => "org.freedesktop.DBus.Error.UnknownInterface",
            Self::UnknownMethod { .. } => "org.freedesktop.DBus.Error.UnknownMethod",
            Self::InvalidArgs { .. } => "org.freedesktop.DBus.Error.InvalidArgs",
            Self::Failed(_) => "org.freedesktop.DBus.Error.Failed",
        }
    }

    /// A human-readable message for the error reply body, the way real
    /// services fill in `org.freedesktop.DBus.Error.*` messages.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownObject { path } => format!("No such object path '{path}'"),
            Self::UnknownInterface { path, interface } => {
                format!("Object '{path}' does not implement interface '{interface}'")
            }
            Self::UnknownMethod { interface, member } => {
                format!("No method '{member}' on interface '{interface}'")
            }
            Self::InvalidArgs {
                interface,
                member,
                expected,
                got,
            } => {
                format!("Call to {interface}.{member} has signature '{got}', expected '{expected}'")
            }
            Self::Failed(message) => message.clone(),
        }
    }
}

/// The result of a successful dispatch: a reply body plus the signature
/// it was declared with, ready for a wire layer to marshal.
#[derive(Debug, Clone, PartialEq)]
pub struct DispatchOutcome {
    pub reply_signature: String,
    pub values: Vec<Value>,
}

/// A single registered method's static shape: the argument signature
/// callers must match, the signature its reply is declared to carry, and
/// the handler itself.
///
/// The handler receives the object path (so one registered
/// `(interface, member)` pair can serve many objects -- e.g. every portal
/// `Session` implements `org.freedesktop.impl.portal.Session.Close`, but
/// which session closes depends on which path was called) plus the
/// decoded argument values, and returns either the reply body values or
/// an application-level failure message.
struct MethodEntry {
    signature: String,
    reply_signature: String,
    handler: Handler,
}

/// A handler that accepts a call and answers it later, from outside the
/// dispatcher. It sees who is calling; the reply is sent by whoever holds
/// the connection.
type DeferredHandler = dyn Fn(&CallContext, &[Value]) -> Result<(), String> + Send + Sync;

enum Handler {
    Immediate(Box<MethodHandler>),
    Deferred(Box<DeferredHandler>),
}

/// What a routed call produced.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    Reply(DispatchOutcome),
    /// The handler accepted the call. Its reply is sent later.
    Deferred,
}

/// A registered method's implementation: given the object path a call
/// landed on and its decoded argument values, return the reply body
/// values or an application-level failure message. Factored into a named
/// alias since the raw `Box<dyn Fn(...) -> ... + Send + Sync>` spelling
/// trips clippy's type-complexity lint and, more importantly, is
/// unreadable repeated at every call site that needs to name this shape.
type MethodHandler = dyn Fn(&str, &[Value]) -> Result<Vec<Value>, String> + Send + Sync;

/// Routes `(interface, member)` method calls to registered handlers.
///
/// Validates the object's interface membership (via an [`ObjectRegistry`]
/// supplied at dispatch time) and the call's argument signature before
/// ever invoking one.
#[derive(Default)]
pub struct MethodCallRouter {
    methods: HashMap<(String, String), MethodEntry>,
}

impl MethodCallRouter {
    /// A router with no methods registered yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for `interface`.`member`. `signature` is the
    /// expected argument signature (e.g. `"ss"`), `reply_signature` the
    /// signature the handler's `Ok` values are declared to carry (e.g.
    /// `"a{sv}"`). Replaces any previously registered handler for the same
    /// `(interface, member)` pair, mirroring
    /// [`ObjectRegistry::register`]'s replace-not-merge behaviour.
    pub fn register<F>(
        &mut self,
        interface: impl Into<String>,
        member: impl Into<String>,
        signature: impl Into<String>,
        reply_signature: impl Into<String>,
        handler: F,
    ) where
        F: Fn(&str, &[Value]) -> Result<Vec<Value>, String> + Send + Sync + 'static,
    {
        self.methods.insert(
            (interface.into(), member.into()),
            MethodEntry {
                signature: signature.into(),
                reply_signature: reply_signature.into(),
                handler: Handler::Immediate(Box::new(handler)),
            },
        );
    }

    /// Register a handler that answers later. `reply_signature` is the
    /// signature of the reply the eventual sender must use.
    pub fn register_deferred<F>(
        &mut self,
        interface: impl Into<String>,
        member: impl Into<String>,
        signature: impl Into<String>,
        reply_signature: impl Into<String>,
        handler: F,
    ) where
        F: Fn(&CallContext, &[Value]) -> Result<(), String> + Send + Sync + 'static,
    {
        self.methods.insert(
            (interface.into(), member.into()),
            MethodEntry {
                signature: signature.into(),
                reply_signature: reply_signature.into(),
                handler: Handler::Deferred(Box::new(handler)),
            },
        );
    }

    /// Dispatch one incoming method call.
    ///
    /// Checks, in the order a real D-Bus service would report them: does
    /// `path` exist at all, does it implement `interface` (every path
    /// implicitly implements [`STANDARD_INTERFACES`] whether or not
    /// [`ObjectRegistry`] was told so -- see that constant's doc comment),
    /// is `member` registered under `interface`, and does `signature`
    /// match what that method expects -- before finally invoking the
    /// handler and wrapping its own failure (if any) as
    /// [`DispatchError::Failed`].
    pub fn dispatch(
        &self,
        registry: &ObjectRegistry,
        path: &str,
        interface: &str,
        member: &str,
        signature: &str,
        args: &[Value],
    ) -> Result<DispatchOutcome, DispatchError> {
        let context = CallContext::anonymous(path);
        match self.dispatch_call(registry, &context, interface, member, signature, args)? {
            CallOutcome::Reply(outcome) => Ok(outcome),
            CallOutcome::Deferred => Err(DispatchError::Failed(format!(
                "{interface}.{member} replies later and needs a caller to answer"
            ))),
        }
    }

    /// Like [`Self::dispatch`], but hands the handler the caller's
    /// [`CallContext`] and allows it to defer the reply.
    pub fn dispatch_call(
        &self,
        registry: &ObjectRegistry,
        context: &CallContext,
        interface: &str,
        member: &str,
        signature: &str,
        args: &[Value],
    ) -> Result<CallOutcome, DispatchError> {
        let path = context.path();
        if !registry.contains(path) {
            return Err(DispatchError::UnknownObject { path: path.into() });
        }
        if !STANDARD_INTERFACES.contains(&interface) && !registry.implements(path, interface) {
            return Err(DispatchError::UnknownInterface {
                path: path.into(),
                interface: interface.into(),
            });
        }
        let entry = self
            .methods
            .get(&(interface.to_string(), member.to_string()))
            .ok_or_else(|| DispatchError::UnknownMethod {
                interface: interface.into(),
                member: member.into(),
            })?;
        if entry.signature != signature {
            return Err(DispatchError::InvalidArgs {
                interface: interface.into(),
                member: member.into(),
                expected: entry.signature.clone(),
                got: signature.into(),
            });
        }
        match &entry.handler {
            Handler::Immediate(handler) => {
                let values = handler(path, args).map_err(DispatchError::Failed)?;
                Ok(CallOutcome::Reply(DispatchOutcome {
                    reply_signature: entry.reply_signature.clone(),
                    values,
                }))
            }
            Handler::Deferred(handler) => {
                handler(context, args).map_err(DispatchError::Failed)?;
                Ok(CallOutcome::Deferred)
            }
        }
    }

    /// How many `(interface, member)` handlers are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.methods.len()
    }

    /// Whether no methods have been registered at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.methods.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS_IFACE: &str = "org.freedesktop.impl.portal.Settings";
    const SETTINGS_PATH: &str = "/org/freedesktop/portal/desktop";

    fn registry_with_settings_object() -> ObjectRegistry {
        let mut registry = ObjectRegistry::new();
        registry.register(SETTINGS_PATH, [SETTINGS_IFACE]);
        registry
    }

    fn router_with_read_all() -> MethodCallRouter {
        let mut router = MethodCallRouter::new();
        router.register(
            SETTINGS_IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            |_path, _args| Ok(vec![Value::Array(vec![])]),
        );
        router
    }

    #[test]
    fn a_fresh_router_has_no_methods() {
        let router = MethodCallRouter::new();
        assert!(router.is_empty());
        assert_eq!(router.len(), 0);
    }

    #[test]
    fn dispatch_reaches_the_standard_introspectable_interface_without_registration() {
        // ObjectRegistry deliberately never tracks
        // org.freedesktop.DBus.Introspectable/Properties (every object
        // answers them implicitly); the router must still let a real
        // Introspect call through rather than reporting UnknownInterface.
        let registry = registry_with_settings_object();
        let mut router = MethodCallRouter::new();
        router.register(
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            "",
            "s",
            |_path, _args| Ok(vec![Value::Str("<node/>".into())]),
        );

        let outcome = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                "org.freedesktop.DBus.Introspectable",
                "Introspect",
                "",
                &[],
            )
            .unwrap();
        assert_eq!(outcome.values, vec![Value::Str("<node/>".into())]);
    }

    #[test]
    fn dispatch_reaches_the_standard_properties_interface_without_registration() {
        let registry = registry_with_settings_object();
        let mut router = MethodCallRouter::new();
        router.register(
            "org.freedesktop.DBus.Properties",
            "GetAll",
            "s",
            "a{sv}",
            |_path, _args| Ok(vec![Value::Array(vec![])]),
        );

        let outcome = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                "s",
                &[Value::Str(SETTINGS_IFACE.into())],
            )
            .unwrap();
        assert_eq!(outcome.values, vec![Value::Array(vec![])]);
    }

    #[test]
    fn dispatch_to_an_unregistered_path_is_unknown_object() {
        let registry = ObjectRegistry::new();
        let router = router_with_read_all();
        let err = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[],
            )
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownObject {
                path: SETTINGS_PATH.into()
            }
        );
        assert_eq!(err.error_name(), "org.freedesktop.DBus.Error.UnknownObject");
    }

    #[test]
    fn dispatch_against_an_unimplemented_interface_is_unknown_interface() {
        let mut registry = ObjectRegistry::new();
        registry.register(SETTINGS_PATH, ["org.freedesktop.impl.portal.Access"]);
        let router = router_with_read_all();

        let err = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[],
            )
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownInterface {
                path: SETTINGS_PATH.into(),
                interface: SETTINGS_IFACE.into(),
            }
        );
    }

    #[test]
    fn dispatch_to_an_unregistered_member_is_unknown_method() {
        let registry = registry_with_settings_object();
        let router = router_with_read_all();

        let err = router
            .dispatch(&registry, SETTINGS_PATH, SETTINGS_IFACE, "Read", "ss", &[])
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnknownMethod {
                interface: SETTINGS_IFACE.into(),
                member: "Read".into(),
            }
        );
    }

    #[test]
    fn dispatch_with_the_wrong_signature_is_invalid_args() {
        let registry = registry_with_settings_object();
        let router = router_with_read_all();

        let err = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "s",
                &[],
            )
            .unwrap_err();
        assert_eq!(
            err,
            DispatchError::InvalidArgs {
                interface: SETTINGS_IFACE.into(),
                member: "ReadAll".into(),
                expected: "as".into(),
                got: "s".into(),
            }
        );
    }

    #[test]
    fn a_matching_call_reaches_the_handler_and_returns_its_reply_signature() {
        let registry = registry_with_settings_object();
        let router = router_with_read_all();

        let outcome = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[Value::Array(vec![])],
            )
            .unwrap();
        assert_eq!(outcome.reply_signature, "a{sa{sv}}");
        assert_eq!(outcome.values, vec![Value::Array(vec![])]);
    }

    #[test]
    fn the_handler_receives_the_calling_path_so_one_registration_can_serve_many_objects() {
        let mut registry = ObjectRegistry::new();
        registry.register("/session/1", ["org.freedesktop.impl.portal.Session"]);
        registry.register("/session/2", ["org.freedesktop.impl.portal.Session"]);

        let mut router = MethodCallRouter::new();
        router.register(
            "org.freedesktop.impl.portal.Session",
            "Close",
            "",
            "",
            |path, _args| Ok(vec![Value::Str(path.to_string())]),
        );

        let outcome = router
            .dispatch(
                &registry,
                "/session/2",
                "org.freedesktop.impl.portal.Session",
                "Close",
                "",
                &[],
            )
            .unwrap();
        assert_eq!(outcome.values, vec![Value::Str("/session/2".into())]);
    }

    fn router_with_deferred_open(queue: crate::call_queue::CallQueue) -> MethodCallRouter {
        let mut router = MethodCallRouter::new();
        router.register_deferred(SETTINGS_IFACE, "Open", "s", "u", move |context, args| {
            queue.push(crate::call_queue::PendingCall {
                context: context.clone(),
                args: args.to_vec(),
            });
            Ok(())
        });
        router
    }

    #[test]
    fn a_deferred_call_is_reported_as_deferred_and_reaches_the_queue_with_its_caller() {
        let registry = registry_with_settings_object();
        let queue = crate::call_queue::CallQueue::new();
        let router = router_with_deferred_open(queue.clone());
        let context = CallContext::new(SETTINGS_PATH, ":1.7", 11);

        let outcome = router
            .dispatch_call(
                &registry,
                &context,
                SETTINGS_IFACE,
                "Open",
                "s",
                &[Value::Str("x".into())],
            )
            .unwrap();

        assert_eq!(outcome, CallOutcome::Deferred);
        let pending = queue.pop().expect("queued");
        assert_eq!(pending.context, context);
        assert_eq!(pending.args, vec![Value::Str("x".into())]);
    }

    #[test]
    fn a_deferred_call_still_checks_its_signature_before_the_handler_runs() {
        let registry = registry_with_settings_object();
        let queue = crate::call_queue::CallQueue::new();
        let router = router_with_deferred_open(queue.clone());
        let context = CallContext::new(SETTINGS_PATH, ":1.7", 11);

        let err = router
            .dispatch_call(&registry, &context, SETTINGS_IFACE, "Open", "u", &[])
            .unwrap_err();

        assert!(matches!(err, DispatchError::InvalidArgs { .. }));
        assert!(queue.is_empty());
    }

    #[test]
    fn a_deferred_handler_failure_becomes_a_failed_dispatch_error() {
        let registry = registry_with_settings_object();
        let mut router = MethodCallRouter::new();
        router.register_deferred(SETTINGS_IFACE, "Open", "", "u", |_context, _args| {
            Err("busy".to_string())
        });
        let context = CallContext::new(SETTINGS_PATH, ":1.7", 11);

        let err = router
            .dispatch_call(&registry, &context, SETTINGS_IFACE, "Open", "", &[])
            .unwrap_err();

        assert_eq!(err, DispatchError::Failed("busy".into()));
    }

    #[test]
    fn plain_dispatch_of_a_deferred_method_is_refused_rather_than_answered_empty() {
        let registry = registry_with_settings_object();
        let router = router_with_deferred_open(crate::call_queue::CallQueue::new());

        let err = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "Open",
                "s",
                &[Value::Str("x".into())],
            )
            .unwrap_err();

        assert!(matches!(err, DispatchError::Failed(_)));
    }

    #[test]
    fn an_immediate_method_through_dispatch_call_replies() {
        let registry = registry_with_settings_object();
        let router = router_with_read_all();
        let context = CallContext::new(SETTINGS_PATH, ":1.7", 11);

        let outcome = router
            .dispatch_call(
                &registry,
                &context,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[Value::Array(vec![])],
            )
            .unwrap();

        assert!(matches!(outcome, CallOutcome::Reply(_)));
    }

    #[test]
    fn a_handler_level_failure_becomes_a_failed_dispatch_error() {
        let registry = registry_with_settings_object();
        let mut router = MethodCallRouter::new();
        router.register(
            SETTINGS_IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            |_path, _args| Err("namespace not found".to_string()),
        );

        let err = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[],
            )
            .unwrap_err();
        assert_eq!(err, DispatchError::Failed("namespace not found".into()));
        assert_eq!(err.error_name(), "org.freedesktop.DBus.Error.Failed");
        assert_eq!(err.message(), "namespace not found");
    }

    #[test]
    fn re_registering_the_same_interface_and_member_replaces_the_handler() {
        let registry = registry_with_settings_object();
        let mut router = MethodCallRouter::new();
        router.register(
            SETTINGS_IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            |_path, _args| Ok(vec![Value::Str("first".into())]),
        );
        router.register(
            SETTINGS_IFACE,
            "ReadAll",
            "as",
            "a{sa{sv}}",
            |_path, _args| Ok(vec![Value::Str("second".into())]),
        );

        assert_eq!(router.len(), 1);
        let outcome = router
            .dispatch(
                &registry,
                SETTINGS_PATH,
                SETTINGS_IFACE,
                "ReadAll",
                "as",
                &[],
            )
            .unwrap();
        assert_eq!(outcome.values, vec![Value::Str("second".into())]);
    }
}
