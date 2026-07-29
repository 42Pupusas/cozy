//! Object lifetime and lookup: which object paths currently exist on this
//! service, and which interfaces each one answers to.
//!
//! Deliberately separate from method dispatch (that's
//! [`crate::router`](../router/index.html), added in the next slice): a
//! portal's `/org/freedesktop/portal/desktop/request/<sender>/<token>`
//! objects are created and destroyed constantly as calls come and go, and
//! that lifecycle bookkeeping has nothing to do with *how* a method call
//! against one of them gets matched to a handler. Keeping them apart means
//! the router can stay a pure "given a live object, dispatch" concern and
//! the registry can stay a pure "is this path alive, what does it claim to
//! implement" concern, each independently testable.

use std::collections::HashMap;

/// One live object: its object path (the map key, not stored again here)
/// and the interfaces it answers to, e.g.
/// `["org.freedesktop.DBus.Properties", "org.freedesktop.impl.portal.Access"]`.
struct ObjectEntry {
    interfaces: Vec<String>,
}

/// A registry of currently-live D-Bus object paths and the interfaces each
/// one implements.
///
/// Every registered object implicitly answers
/// `org.freedesktop.DBus.Introspectable` and `org.freedesktop.DBus.Properties`
/// (per spec, any object with properties or children must); this registry
/// only tracks the interfaces a caller explicitly registers, leaving those
/// two spec-mandated ones to whichever layer builds introspection XML or a
/// `Properties` reply, since neither needs the registry's confirmation that
/// they're supported.
#[derive(Default)]
pub struct ObjectRegistry {
    objects: HashMap<String, ObjectEntry>,
}

impl ObjectRegistry {
    /// An empty registry, i.e. a service that hasn't published anything on
    /// the bus yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish an object at `path` implementing `interfaces`. Replaces
    /// whatever interface list was previously registered at `path`, if any
    /// -- re-registering the same path is how a caller updates an object's
    /// advertised interfaces without a separate "unregister then register"
    /// dance.
    pub fn register<P, I, S>(&mut self, path: P, interfaces: I)
    where
        P: Into<String>,
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let interfaces = interfaces.into_iter().map(Into::into).collect();
        self.objects.insert(path.into(), ObjectEntry { interfaces });
    }

    /// Remove `path` from the registry, e.g. when a portal `Request` or
    /// `Session` object is closed. Returns whether it was actually present.
    pub fn unregister(&mut self, path: &str) -> bool {
        self.objects.remove(path).is_some()
    }

    /// Whether `path` currently has anything registered.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.objects.contains_key(path)
    }

    /// The interfaces registered at `path`, or `None` if `path` doesn't
    /// exist. Distinct from an empty slice, which would mean the path
    /// exists but was registered with zero interfaces.
    #[must_use]
    pub fn interfaces_of(&self, path: &str) -> Option<&[String]> {
        self.objects
            .get(path)
            .map(|entry| entry.interfaces.as_slice())
    }

    /// Whether `path` exists and claims to implement `interface`.
    #[must_use]
    pub fn implements(&self, path: &str, interface: &str) -> bool {
        self.interfaces_of(path)
            .is_some_and(|interfaces| interfaces.iter().any(|i| i == interface))
    }

    /// Every currently-registered object path, in no particular order --
    /// e.g. for a housekeeping sweep that closes every `Request` object
    /// still open when its owning connection drops.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.objects.keys().map(String::as_str)
    }

    /// How many objects are currently registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the registry currently has no objects at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_registry_is_empty() {
        let registry = ObjectRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(!registry.contains("/org/freedesktop/portal/desktop"));
    }

    #[test]
    fn registering_an_object_makes_it_visible_by_path_and_interface() {
        let mut registry = ObjectRegistry::new();
        registry.register(
            "/org/freedesktop/portal/desktop",
            ["org.freedesktop.impl.portal.Settings"],
        );

        assert!(registry.contains("/org/freedesktop/portal/desktop"));
        assert_eq!(registry.len(), 1);
        assert!(registry.implements(
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.impl.portal.Settings"
        ));
        assert!(!registry.implements(
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.impl.portal.Access"
        ));
    }

    #[test]
    fn an_unregistered_path_reports_no_interfaces_and_is_absent() {
        let registry = ObjectRegistry::new();
        assert_eq!(registry.interfaces_of("/nope"), None);
        assert!(!registry.implements("/nope", "org.freedesktop.DBus.Properties"));
    }

    #[test]
    fn re_registering_a_path_replaces_its_interface_list_rather_than_merging() {
        let mut registry = ObjectRegistry::new();
        registry.register("/obj", ["a.First"]);
        registry.register("/obj", ["a.Second"]);

        assert!(!registry.implements("/obj", "a.First"));
        assert!(registry.implements("/obj", "a.Second"));
        assert_eq!(
            registry.interfaces_of("/obj"),
            Some(["a.Second".to_string()].as_slice())
        );
    }

    #[test]
    fn unregister_removes_the_object_and_reports_whether_it_existed() {
        let mut registry = ObjectRegistry::new();
        registry.register("/obj", Vec::<String>::new());

        assert!(registry.unregister("/obj"));
        assert!(!registry.contains("/obj"));
        // Removing again reports false rather than panicking -- callers
        // sweeping up after a disconnected client shouldn't have to check
        // existence first.
        assert!(!registry.unregister("/obj"));
    }

    #[test]
    fn a_registered_object_may_have_zero_interfaces() {
        // Distinguishes "path exists, implements nothing extra beyond the
        // spec-mandated interfaces" from "path was never registered".
        let mut registry = ObjectRegistry::new();
        registry.register("/obj", Vec::<String>::new());

        assert!(registry.contains("/obj"));
        assert_eq!(registry.interfaces_of("/obj"), Some([].as_slice()));
    }

    #[test]
    fn paths_lists_every_registered_object() {
        let mut registry = ObjectRegistry::new();
        registry.register("/a", ["x.A"]);
        registry.register("/b", ["x.B"]);

        let mut paths: Vec<&str> = registry.paths().collect();
        paths.sort_unstable();
        assert_eq!(paths, vec!["/a", "/b"]);
    }
}
