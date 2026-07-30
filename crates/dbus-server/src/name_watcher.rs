//! Caller-death cleanup.
//!
//! Which unique connection name owns which server-side resource (a
//! `Request`/`Session` object path, most often), and turning a
//! `NameOwnerChanged` signal into the list of resources to tear down when
//! that connection disappears.
//!
//! Portal `Request` and `Session` objects must be closed the moment the
//! client that created them disconnects without saying so explicitly --
//! clients never call `Session.Close` on their way out, they just die.
//! `org.freedesktop.DBus`'s `NameOwnerChanged(name, old_owner, new_owner)`
//! broadcast with an empty `new_owner` is the only reliable "this
//! connection is gone" signal on the bus -- decoding and interpreting the
//! signal itself is [`dbus::NameOwnerChanged`]'s job (shared with
//! `statusbar::tray::TrayWatcher`'s `drop_items_for_lost_owner`, which
//! relies on the exact same rule for tray items); this module gives the
//! portal daemon the resource-tracking side, generic over resource type.
//!
//! Deliberately just a name -> resource map, not a subscription to a live
//! [`dbus::conn::DbusConn`]: this crate's other pieces
//! ([`crate::registry::ObjectRegistry`], [`crate::properties::PropertyTable`])
//! don't own a connection either, so `bins/portal`'s wiring is the one
//! place that both holds a `DbusConn` and needs to react to
//! `NameWatcher::forget_owner`'s result by actually unregistering those
//! paths.

use std::collections::HashMap;

/// Tracks which unique connection name (e.g. `:1.55`) currently owns which
/// resource identifiers (typically object paths), and answers "this
/// connection just died, what does that take down with it".
#[derive(Default)]
pub struct NameWatcher {
    /// owner -> every resource it currently owns, in registration order
    /// (oldest first) so a caller tearing things down in the order they
    /// were created -- outermost `Session` before an inner `Request` it
    /// spawned, say -- gets a stable, predictable list rather than
    /// whatever a hash map's iteration order happens to be.
    resources_by_owner: HashMap<String, Vec<String>>,
    /// resource -> owner, the reverse index [`Self::untrack`] and
    /// re-[`Self::track`]ing need to find (and remove) a resource's
    /// current entry in `resources_by_owner` without a linear scan.
    owner_by_resource: HashMap<String, String>,
}

impl NameWatcher {
    /// A watcher tracking nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `owner` (a unique connection name) owns `resource`.
    /// If `resource` was already tracked under a different owner (a
    /// caller re-using the same identifier is a bug elsewhere, but this
    /// stays total rather than leaving stale state), it's moved rather
    /// than duplicated.
    pub fn track(&mut self, owner: impl Into<String>, resource: impl Into<String>) {
        let owner = owner.into();
        let resource = resource.into();
        if let Some(previous_owner) = self.owner_by_resource.get(&resource)
            && previous_owner != &owner
        {
            let previous_owner = previous_owner.clone();
            if let Some(list) = self.resources_by_owner.get_mut(&previous_owner) {
                list.retain(|r| r != &resource);
            }
        }
        self.owner_by_resource
            .insert(resource.clone(), owner.clone());
        let list = self.resources_by_owner.entry(owner).or_default();
        if !list.contains(&resource) {
            list.push(resource);
        }
    }

    /// Stop tracking `resource` (e.g. the portal itself closed the
    /// `Session` in response to an explicit `Close` call, so its owner's
    /// eventual disconnect shouldn't try to tear it down again). Returns
    /// whether it was tracked.
    pub fn untrack(&mut self, resource: &str) -> bool {
        let Some(owner) = self.owner_by_resource.remove(resource) else {
            return false;
        };
        if let Some(list) = self.resources_by_owner.get_mut(&owner) {
            list.retain(|r| r != resource);
        }
        true
    }

    /// The owner currently tracked for `resource`, if any.
    #[must_use]
    pub fn owner_of(&self, resource: &str) -> Option<&str> {
        self.owner_by_resource.get(resource).map(String::as_str)
    }

    /// Every resource currently tracked under `owner`, oldest first, or an
    /// empty slice if `owner` isn't tracked at all.
    #[must_use]
    pub fn resources_of(&self, owner: &str) -> &[String] {
        self.resources_by_owner
            .get(owner)
            .map_or(&[], Vec::as_slice)
    }

    /// Stop tracking every resource `owner` held and return them, oldest
    /// first -- the list a caller should unregister (from
    /// [`crate::registry::ObjectRegistry`] and anywhere else that cares)
    /// once [`dbus::NameOwnerChanged::lost_owner`] reports `owner` has
    /// disconnected.
    pub fn forget_owner(&mut self, owner: &str) -> Vec<String> {
        let Some(resources) = self.resources_by_owner.remove(owner) else {
            return Vec::new();
        };
        for resource in &resources {
            self.owner_by_resource.remove(resource);
        }
        resources
    }

    /// How many owners currently have at least one tracked resource.
    #[must_use]
    pub fn owner_count(&self) -> usize {
        self.resources_by_owner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = ":1.55";
    const OTHER_OWNER: &str = ":1.5";
    const SESSION_PATH: &str = "/org/freedesktop/portal/desktop/session/1_55/settings";
    const REQUEST_PATH: &str = "/org/freedesktop/portal/desktop/request/1_55/token";

    #[test]
    fn a_fresh_watcher_tracks_nothing() {
        let watcher = NameWatcher::new();
        assert_eq!(watcher.owner_count(), 0);
        assert_eq!(watcher.owner_of(SESSION_PATH), None);
        assert!(watcher.resources_of(OWNER).is_empty());
    }

    #[test]
    fn tracking_a_resource_makes_it_visible_both_ways() {
        let mut watcher = NameWatcher::new();
        watcher.track(OWNER, SESSION_PATH);

        assert_eq!(watcher.owner_of(SESSION_PATH), Some(OWNER));
        assert_eq!(watcher.resources_of(OWNER), [SESSION_PATH.to_string()]);
    }

    #[test]
    fn one_owner_can_hold_several_resources_in_registration_order() {
        let mut watcher = NameWatcher::new();
        watcher.track(OWNER, SESSION_PATH);
        watcher.track(OWNER, REQUEST_PATH);

        assert_eq!(
            watcher.resources_of(OWNER),
            [SESSION_PATH.to_string(), REQUEST_PATH.to_string()]
        );
    }

    #[test]
    fn untrack_removes_only_the_named_resource() {
        let mut watcher = NameWatcher::new();
        watcher.track(OWNER, SESSION_PATH);
        watcher.track(OWNER, REQUEST_PATH);

        assert!(watcher.untrack(SESSION_PATH));
        assert_eq!(watcher.owner_of(SESSION_PATH), None);
        assert_eq!(watcher.resources_of(OWNER), [REQUEST_PATH.to_string()]);
    }

    #[test]
    fn untrack_on_an_unknown_resource_reports_false() {
        let mut watcher = NameWatcher::new();
        assert!(!watcher.untrack("/nope"));
    }

    #[test]
    fn re_tracking_a_resource_under_a_new_owner_moves_it() {
        let mut watcher = NameWatcher::new();
        watcher.track(OTHER_OWNER, SESSION_PATH);
        watcher.track(OWNER, SESSION_PATH);

        assert_eq!(watcher.owner_of(SESSION_PATH), Some(OWNER));
        assert!(watcher.resources_of(OTHER_OWNER).is_empty());
        assert_eq!(watcher.resources_of(OWNER), [SESSION_PATH.to_string()]);
    }

    #[test]
    fn forget_owner_removes_and_returns_everything_it_held() {
        let mut watcher = NameWatcher::new();
        watcher.track(OWNER, SESSION_PATH);
        watcher.track(OWNER, REQUEST_PATH);

        let forgotten = watcher.forget_owner(OWNER);
        assert_eq!(
            forgotten,
            vec![SESSION_PATH.to_string(), REQUEST_PATH.to_string()]
        );
        assert_eq!(watcher.owner_of(SESSION_PATH), None);
        assert_eq!(watcher.owner_of(REQUEST_PATH), None);
        assert_eq!(watcher.owner_count(), 0);
    }

    #[test]
    fn forget_owner_on_an_untracked_owner_returns_empty_without_panicking() {
        let mut watcher = NameWatcher::new();
        assert_eq!(watcher.forget_owner(OWNER), Vec::<String>::new());
    }
}
