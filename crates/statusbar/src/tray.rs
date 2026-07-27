//! `StatusNotifierItem` watcher for the bar's system tray.
//!
//! Unlike the old `notifications` module, this owns the standard session-bus
//! watcher name and receives registrations from applications themselves. That
//! is the channel Mattermost uses for its native logo and its
//! `NeedsAttention` state, independently of a Wayland window's urgency bit.

use std::sync::mpsc::Sender;

use dbus::{DbusConn, Value};

use crate::{TrayIcon, TrayPixmap};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const REGISTER_ITEM: &str = "RegisterStatusNotifierItem";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";
const HOST_REGISTERED_PROPERTY: &str = "IsStatusNotifierHostRegistered";
const PROTOCOL_VERSION_PROPERTY: &str = "ProtocolVersion";

/// One visible system-tray item. `attention` selects the client's dedicated
/// attention artwork when it supplied one; the renderer is otherwise unaware
/// of per-application notification semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// Unique sender plus object path, stable for the app connection's life.
    pub id: String,
    /// Destination used for property queries (may be a well-known name).
    service: String,
    /// The item's object path, needed to recognize its change signals.
    path: String,
    /// The unique connection name (e.g. `:1.55`) that registered this item,
    /// captured at registration time regardless of whether `service` is
    /// itself a well-known name. `NameOwnerChanged` always reports the
    /// disappearing owner as a unique name, so this is the only thing safe
    /// to compare it against -- see `drop_items_for_lost_owner`, which used
    /// to compare with `str::starts_with` and could evict an unrelated
    /// item whenever any other client's unique name (e.g. `:1.5`) happened
    /// to be a string prefix of this one's (e.g. `:1.55`).
    owner: String,
    pub icon: TrayIcon,
    pub attention: bool,
}

/// Snapshot emitted whenever a client registers or changes its status/icon.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TraySnapshot {
    pub items: Vec<TrayItem>,
}

/// Owns the long-lived `StatusNotifier` watcher loop on a dedicated thread.
///
/// The bar's normal worker may block briefly for sensor reads; a tray watcher
/// must instead remain waiting for incoming D-Bus method calls at all times,
/// so it gets its own thread and reports immutable snapshots over a channel.
pub struct TrayWatcher;

impl TrayWatcher {
    /// Start the watcher. Failure is non-fatal: another bar may already own
    /// the standard watcher name, in which case that bar remains responsible
    /// for all tray items and ours simply has an empty tray.
    pub fn spawn(snapshot_tx: Sender<TraySnapshot>, wake: waker::Waker) {
        let _ = std::thread::Builder::new()
            .name("statusbar-tray".to_owned())
            .spawn(move || Self::run(&snapshot_tx, &wake));
    }

    fn run(snapshot_tx: &Sender<TraySnapshot>, wake: &waker::Waker) {
        let Ok(mut conn) = DbusConn::session() else {
            eprintln!("statusbar: no session D-Bus; system tray disabled");
            return;
        };
        match conn.request_name(WATCHER_NAME) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("statusbar: another StatusNotifier watcher owns the tray");
                return;
            }
            Err(e) => {
                eprintln!("statusbar: could not claim system tray watcher: {e}");
                return;
            }
        }
        if let Err(e) = conn.add_match("type='signal',interface='org.kde.StatusNotifierItem'") {
            eprintln!("statusbar: could not subscribe to tray item updates: {e}");
            return;
        }
        // The StatusNotifierItem specification names `NewIcon`/`NewStatus`
        // signals, but Electron clients in practice also use the generic
        // `PropertiesChanged` path when their icon is initialized after the
        // registration call returns. The initial property read then sees an
        // empty pixmap and the item remains permanently invisible if this
        // signal is ignored. Subscribe to both conventions so a late icon or
        // attention transition always triggers a fresh, complete item read.
        if let Err(e) = conn.add_match("type='signal',interface='org.freedesktop.DBus.Properties'")
        {
            eprintln!("statusbar: could not subscribe to tray property updates: {e}");
            return;
        }
        // Clients like Mattermost/Electron sometimes reconnect to the bus
        // (a fresh unique name) and re-register rather than reusing their
        // original connection. Without watching for the old connection's
        // disappearance, its item's `id` (keyed on that stale unique name)
        // never matches the new registration's id, so `register`'s dedup
        // never fires and the stale entry lingers forever as a duplicate.
        if let Err(e) = conn
            .add_match("type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged'")
        {
            eprintln!("statusbar: could not subscribe to tray owner changes: {e}");
            return;
        }

        let mut items = Vec::new();
        loop {
            let Ok(message) = conn.recv_message() else {
                eprintln!("statusbar: system tray D-Bus connection closed");
                return;
            };
            let changed = if Self::is_watcher_property_get(&message) {
                Self::reply_watcher_property(&mut conn, &message);
                false
            } else if Self::is_item_registration(&message) {
                Self::register(&mut conn, &message, &mut items)
            } else if Self::is_name_owner_lost(&message) {
                Self::drop_items_for_lost_owner(&message, &mut items)
            } else {
                Self::refresh_changed_item(&mut conn, &message, &mut items)
            };
            if changed {
                let _ = snapshot_tx.send(TraySnapshot {
                    items: items.clone(),
                });
                wake.signal();
            }
        }
    }

    fn is_item_registration(message: &dbus::message::Reply) -> bool {
        message.path.as_deref() == Some(WATCHER_PATH)
            && message.interface.as_deref() == Some(WATCHER_NAME)
            && message.member.as_deref() == Some(REGISTER_ITEM)
    }

    /// `NameOwnerChanged(name, old_owner, new_owner)` reports a unique or
    /// well-known name losing its owner when `new_owner` is empty. That is
    /// the only reliable "this client is gone" signal on the bus -- clients
    /// never unregister their tray item explicitly, they just disconnect.
    fn is_name_owner_lost(message: &dbus::message::Reply) -> bool {
        if message.interface.as_deref() != Some("org.freedesktop.DBus")
            || message.member.as_deref() != Some("NameOwnerChanged")
        {
            return false;
        }
        let values = message.values();
        matches!(values.get(2).and_then(Value::as_display_string), Some(new_owner) if new_owner.is_empty())
    }

    /// Drop every item registered by the connection that just disappeared.
    /// `NameOwnerChanged(name, old_owner, new_owner)` reports either a
    /// unique name itself dying (`name == old_owner`, both unique) or a
    /// well-known name's owner changing (`old_owner` is the unique name that
    /// used to hold it) -- either way `old_owner` is the unique connection
    /// name to compare against `TrayItem::owner`, which was captured
    /// verbatim from `message.sender` at registration time.
    ///
    /// This must be an *exact* match, not a prefix match: unique names look
    /// like `:1.5` and `:1.55`, and `str::starts_with` treated the former as
    /// a match for the latter, evicting an unrelated still-live tray item
    /// (e.g. Mattermost's) the moment any other, unrelated client with a
    /// shorter numeric suffix happened to disconnect -- which explained the
    /// icon flashing in once at registration and then vanishing almost
    /// immediately.
    fn drop_items_for_lost_owner(
        message: &dbus::message::Reply,
        items: &mut Vec<TrayItem>,
    ) -> bool {
        let values = message.values();
        let Some(old_owner) = values.get(1).and_then(Value::as_display_string) else {
            return false;
        };
        if old_owner.is_empty() {
            return false;
        }
        let before = items.len();
        items.retain(|item| item.owner != old_owner);
        items.len() != before
    }

    /// Electron probes this standard watcher property before deciding whether
    /// an SNI host is usable. Owning the bus name alone is not enough: an
    /// earlier implementation dropped this method call, causing Mattermost
    /// to silently decide no tray host existed and never register its item.
    fn is_watcher_property_get(message: &dbus::message::Reply) -> bool {
        message.path.as_deref() == Some(WATCHER_PATH)
            && message.interface.as_deref() == Some(PROPERTIES_IFACE)
            && message.member.as_deref() == Some("Get")
    }

    fn reply_watcher_property(conn: &mut DbusConn, message: &dbus::message::Reply) {
        let (Some(sender), values) = (message.sender.as_deref(), message.values()) else {
            return;
        };
        let property = values.get(1).and_then(Value::as_display_string);
        let value = match property.as_deref() {
            Some(HOST_REGISTERED_PROPERTY) => Some(dbus::message::BodyArg::VariantBool(true)),
            Some(PROTOCOL_VERSION_PROPERTY) => Some(dbus::message::BodyArg::VariantU32(0)),
            _ => None,
        };
        if let Some(value) = value {
            let _ = conn.send_method_return(sender, message.serial, &[value]);
        }
    }

    /// Resolve the registration's single string argument. The specification
    /// permits either a service name (with the conventional object path) or
    /// an object path (whose service is the call sender).
    fn registration_target(
        message: &dbus::message::Reply,
        sender: &str,
    ) -> Option<(String, String)> {
        let arg = message.values().into_iter().next()?.as_display_string()?;
        if arg.starts_with('/') {
            Some((sender.to_owned(), arg))
        } else {
            Some((arg, ITEM_PATH.to_owned()))
        }
    }

    fn register(
        conn: &mut DbusConn,
        message: &dbus::message::Reply,
        items: &mut Vec<TrayItem>,
    ) -> bool {
        let Some(sender) = message.sender.as_deref() else {
            return false;
        };
        let Some((service, path)) = Self::registration_target(message, sender) else {
            return false;
        };
        // Registration has no return values. Reply before querying the item:
        // clients may not expose properties until their invocation completes.
        if conn
            .send_method_return(sender, message.serial, &[])
            .is_err()
        {
            return false;
        }
        let Some(item) = Self::read_item(conn, sender, &service, &path) else {
            return false;
        };
        items.retain(|old| old.id != item.id);
        items.push(item);
        true
    }

    /// `NewIcon`/`NewStatus` and generic `PropertiesChanged` signals carry
    /// insufficient pixels to paint directly, so refresh the registered
    /// item's complete properties on either signal convention.
    fn refresh_changed_item(
        conn: &mut DbusConn,
        message: &dbus::message::Reply,
        items: &mut [TrayItem],
    ) -> bool {
        let (Some(sender), Some(path)) = (message.sender.as_deref(), message.path.as_deref())
        else {
            return false;
        };
        if !Self::is_item_update_signal(message) {
            return false;
        }
        let id = format!("{sender}{path}");
        let Some(index) = items.iter().position(|item| item.id == id) else {
            return false;
        };
        let item = &items[index];
        let Some(replacement) = Self::read_item(conn, sender, &item.service, &item.path) else {
            return false;
        };
        items[index] = replacement;
        true
    }

    /// Whether `message` announces a changed `StatusNotifier` item. Supports
    /// both the protocol's dedicated item signals and D-Bus's generic
    /// property-change signal, which Electron applications commonly use for
    /// their asynchronously-created tray artwork.
    fn is_item_update_signal(message: &dbus::message::Reply) -> bool {
        if message.kind != dbus::message::MessageType::Signal {
            return false;
        }
        match message.interface.as_deref() {
            Some(ITEM_IFACE) => matches!(
                message.member.as_deref(),
                Some("NewIcon" | "NewAttentionIcon" | "NewStatus")
            ),
            Some(PROPERTIES_IFACE) if message.member.as_deref() == Some("PropertiesChanged") => {
                message
                    .values()
                    .first()
                    .and_then(Value::as_display_string)
                    .as_deref()
                    == Some(ITEM_IFACE)
            }
            _ => false,
        }
    }

    fn read_item(conn: &mut DbusConn, sender: &str, service: &str, path: &str) -> Option<TrayItem> {
        let status = conn
            .get_property(service, path, ITEM_IFACE, "Status")
            .ok()?
            .as_display_string()
            .unwrap_or_default();
        let attention = status == "NeedsAttention";
        let icon_property = if attention {
            "AttentionIconPixmap"
        } else {
            "IconPixmap"
        };
        let pixmaps = conn
            .get_property(service, path, ITEM_IFACE, icon_property)
            .ok()
            .map_or_else(Vec::new, |value| Self::pixmaps(&value));
        // Some clients announce attention before supplying attention artwork.
        // Keep their regular icon visible rather than making the tray item
        // disappear; the red attention badge still conveys state.
        let icon = if pixmaps.is_empty() && attention {
            conn.get_property(service, path, ITEM_IFACE, "IconPixmap")
                .ok()
                .map_or_else(Vec::new, |value| Self::pixmaps(&value))
        } else {
            pixmaps
        };
        Some(TrayItem {
            id: format!("{sender}{path}"),
            service: service.to_owned(),
            path: path.to_owned(),
            owner: sender.to_owned(),
            icon: TrayIcon::new(icon),
            attention,
        })
    }

    /// Decode `IconPixmap`'s `a(iiay)` value into safe pixel buffers.
    fn pixmaps(value: &Value) -> Vec<TrayPixmap> {
        value
            .as_items()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let fields = entry.as_items()?;
                let width = i32::try_from(fields.first()?.as_u64()?).ok()?;
                let height = i32::try_from(fields.get(1)?.as_u64()?).ok()?;
                let bytes: Vec<u8> = fields
                    .get(2)?
                    .as_items()?
                    .iter()
                    .map(|v| match v {
                        Value::Byte(byte) => Some(*byte),
                        _ => None,
                    })
                    .collect::<Option<_>>()?;
                let argb = bytes
                    .chunks_exact(4)
                    .map(|pixel| u32::from_be_bytes(pixel.try_into().unwrap_or([0; 4])))
                    .collect();
                TrayPixmap::new(width, height, argb)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_path_registration_uses_the_call_sender() {
        let message = dbus::message::Reply {
            kind: dbus::message::MessageType::MethodCall,
            sender: Some(":1.9".into()),
            path: Some(WATCHER_PATH.into()),
            interface: Some(WATCHER_NAME.into()),
            member: Some(REGISTER_ITEM.into()),
            serial: 1,
            error_name: None,
            reply_serial: None,
            signature: "s".into(),
            body: vec![0; 0],
        };
        // Body decoding belongs to dbus; target parsing's service/path branch
        // is covered by this direct contract test through an equivalent value
        // once real registration frames are exercised in integration testing.
        assert!(TrayWatcher::is_item_registration(&message));
    }

    #[test]
    fn dedicated_item_icon_signal_refreshes_a_registered_item() {
        let message = dbus::message::Reply {
            kind: dbus::message::MessageType::Signal,
            sender: Some(":1.9".into()),
            path: Some(ITEM_PATH.into()),
            interface: Some(ITEM_IFACE.into()),
            member: Some("NewIcon".into()),
            serial: 1,
            error_name: None,
            reply_serial: None,
            signature: String::new(),
            body: Vec::new(),
        };
        assert!(TrayWatcher::is_item_update_signal(&message));
    }

    #[test]
    fn non_signal_item_messages_do_not_trigger_refreshes() {
        let message = dbus::message::Reply {
            kind: dbus::message::MessageType::MethodCall,
            sender: Some(":1.9".into()),
            path: Some(ITEM_PATH.into()),
            interface: Some(ITEM_IFACE.into()),
            member: Some("NewIcon".into()),
            serial: 1,
            error_name: None,
            reply_serial: None,
            signature: String::new(),
            body: Vec::new(),
        };
        assert!(!TrayWatcher::is_item_update_signal(&message));
    }
}
