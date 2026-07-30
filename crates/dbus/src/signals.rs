//! Typed decode-side wrappers for D-Bus signal bodies this workspace's
//! server and client code both need to interpret positionally.
//!
//! `org.freedesktop.DBus.NameOwnerChanged` is the motivating (and so far
//! only) case: before this module existed, `dbus_server::name_watcher::
//! NameWatcher` and `statusbar::tray::TrayWatcher` each hand-decoded the
//! same three positional `Value::Str` arguments (`values.get(1)`,
//! `values.get(2)`, …) independently, with the exact same "only an empty
//! `new_owner` means a real disconnect" rule spelled out twice. A
//! `NameOwnerChanged` decoded once here means both callers work with named
//! fields instead of re-deriving that positional/interpretation logic, and
//! it's the one place either caller's dependency on [`Value`]'s recursive
//! shape actually funnels through.

use crate::value::Value;

/// A decoded `org.freedesktop.DBus.NameOwnerChanged(name, old_owner,
/// new_owner)` signal body.
///
/// Broadcast whenever a bus name's ownership changes. This is the only
/// reliable "this connection is gone" signal on the bus: neither an SNI
/// tray client nor a portal session ever calls anything on its way out,
/// they just disconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameOwnerChanged {
    pub name: String,
    pub old_owner: String,
    pub new_owner: String,
}

impl NameOwnerChanged {
    /// Decode a signal body's three positional, string-like arguments.
    /// Returns `None` if `values` is short or any of the three isn't
    /// string-like — a malformed body from a well-behaved bus should
    /// never happen, but this stays total rather than panicking on a
    /// hypothetical adversarial peer.
    #[must_use]
    pub fn from_body(values: &[Value]) -> Option<Self> {
        Some(Self {
            name: values.first()?.as_display_string()?,
            old_owner: values.get(1)?.as_display_string()?,
            new_owner: values.get(2)?.as_display_string()?,
        })
    }

    /// The unique connection name that just disappeared, if this signal
    /// reports a genuine disconnect rather than a name changing hands.
    ///
    /// Only an empty `new_owner` means the owner is truly gone (a
    /// non-empty one means the well-known name just changed hands, e.g. a
    /// service restarting — the *previous* owner may still be alive and
    /// own other resources, so that isn't a disconnect in the sense
    /// callers here care about). An empty `old_owner` means the name
    /// never had an owner to lose in the first place — the acquisition
    /// half of the same signal, not a loss.
    #[must_use]
    pub fn lost_owner(&self) -> Option<&str> {
        (!self.old_owner.is_empty() && self.new_owner.is_empty()).then_some(self.old_owner.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_body_decodes_the_three_positional_strings() {
        let values = vec![
            Value::Str("org.example.App".into()),
            Value::Str(":1.5".into()),
            Value::Str(":1.6".into()),
        ];
        let decoded = NameOwnerChanged::from_body(&values).unwrap();
        assert_eq!(decoded.name, "org.example.App");
        assert_eq!(decoded.old_owner, ":1.5");
        assert_eq!(decoded.new_owner, ":1.6");
    }

    #[test]
    fn from_body_is_none_on_a_short_or_malformed_body() {
        assert!(NameOwnerChanged::from_body(&[]).is_none());
        assert!(NameOwnerChanged::from_body(&[Value::U32(1)]).is_none());
        assert!(
            NameOwnerChanged::from_body(&[
                Value::Str("org.example.App".into()),
                Value::Str(":1.5".into()),
            ])
            .is_none()
        );
    }

    #[test]
    fn lost_owner_reports_the_old_owner_when_new_owner_is_empty() {
        let changed = NameOwnerChanged {
            name: "org.example.App".into(),
            old_owner: ":1.5".into(),
            new_owner: String::new(),
        };
        assert_eq!(changed.lost_owner(), Some(":1.5"));
    }

    #[test]
    fn lost_owner_is_none_when_the_name_just_changed_hands() {
        // Both old and new non-empty: the previous owner may still be
        // alive and own other resources, so this isn't the disconnect a
        // caller here cares about.
        let changed = NameOwnerChanged {
            name: "org.example.App".into(),
            old_owner: ":1.5".into(),
            new_owner: ":1.6".into(),
        };
        assert_eq!(changed.lost_owner(), None);
    }

    #[test]
    fn lost_owner_is_none_when_the_name_never_had_an_owner() {
        // This is the acquisition half of the same signal, not a loss.
        let changed = NameOwnerChanged {
            name: "org.example.App".into(),
            old_owner: String::new(),
            new_owner: ":1.5".into(),
        };
        assert_eq!(changed.lost_owner(), None);
    }
}
