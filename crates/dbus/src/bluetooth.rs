//! `org.bluez` panel surface.
//!
//! Adapter power, discovery, and the paired + nearby device list —
//! everything a "scan and connect" popup needs, beyond what the
//! always-on bar module's [`crate::bluez`] (power + connected count)
//! reads.
//!
//! Same walk as [`crate::bluez::BluetoothStatus::query`]
//! (`GetManagedObjects`, one round trip for the whole object tree), just
//! keeping every device instead of collapsing them into a count.

use crate::conn::{DbusConn, DbusError};
use crate::message::{MessageType, Reply};
use crate::value::Value;

const DEST: &str = "org.bluez";
const ROOT_PATH: &str = "/";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// One bluetooth device object: address, display name, type icon, and its
/// pairing/connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub address: String,
    pub name: String,
    /// bluez's own device-class icon name (`"audio-headsets"`,
    /// `"input-mouse"`, ...) — used directly as the panel's type label.
    pub icon: String,
    pub paired: bool,
    pub connected: bool,
    /// bluez object path — the token connect/disconnect/pair calls key
    /// their requests on.
    pub path: String,
}

/// The machine's bluetooth adapter (or `None`), matching
/// [`crate::bluez::BluetoothStatus`]'s "no adapter" convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothAdapter {
    pub path: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discovering: bool,
}

impl BluetoothAdapter {
    /// Read every adapter property in one `GetAll` call.
    fn from_properties(path: &str, props: &Value) -> Self {
        Self {
            path: path.to_owned(),
            powered: props
                .dict_get("Powered")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            discoverable: props
                .dict_get("Discoverable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            discovering: props
                .dict_get("Discovering")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }
}

impl BluetoothDevice {
    /// Read the display-relevant device properties out of one
    /// `a{sa{sv}}` managed-objects entry.
    fn from_properties(path: &str, props: &Value) -> Self {
        Self {
            address: props
                .dict_get("Address")
                .and_then(Value::as_display_string)
                .unwrap_or_default(),
            name: props
                .dict_get("Name")
                .and_then(Value::as_display_string)
                .unwrap_or_default(),
            icon: props
                .dict_get("Icon")
                .and_then(Value::as_display_string)
                .unwrap_or_default(),
            paired: props
                .dict_get("Paired")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            connected: props
                .dict_get("Connected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            path: path.to_owned(),
        }
    }
}

/// Marker type owning the panel-facing bluez calls.
pub struct BluetoothPanel;

impl BluetoothPanel {
    /// The match rule that delivers every signal bluez emits: property
    /// changes and objects appearing or vanishing.
    pub const SIGNAL_RULE: &'static str = "type='signal',sender='org.bluez'";

    /// The properties the panel displays. A `PropertiesChanged` that
    /// touches none of them (`RSSI` updates during discovery, mostly) leaves
    /// the panel's picture unchanged.
    const DISPLAYED_PROPERTIES: [&'static str; 8] = [
        "Address",
        "Name",
        "Icon",
        "Paired",
        "Connected",
        "Powered",
        "Discoverable",
        "Discovering",
    ];

    /// Subscribe `conn` to everything bluez announces.
    pub fn subscribe(conn: &mut DbusConn) -> Result<(), DbusError> {
        conn.add_match(Self::SIGNAL_RULE)
    }

    /// Whether `message` is a bluez signal after which the panel's device
    /// and adapter state must be read again.
    #[must_use]
    pub fn changes_panel_state(message: &Reply) -> bool {
        if message.kind != MessageType::Signal {
            return false;
        }
        match (message.interface.as_deref(), message.member.as_deref()) {
            (Some(OBJECT_MANAGER_IFACE), Some("InterfacesAdded" | "InterfacesRemoved")) => true,
            (Some(PROPERTIES_IFACE), Some("PropertiesChanged")) => {
                Self::properties_touch_panel(&message.values())
            }
            _ => false,
        }
    }

    /// `PropertiesChanged`'s `(s, a{sv}, as)` body: does it change a
    /// displayed property of an adapter or a device?
    fn properties_touch_panel(body: &[Value]) -> bool {
        let Some(interface) = body.first().and_then(Value::as_display_string) else {
            return false;
        };
        if interface != ADAPTER_IFACE && interface != DEVICE_IFACE {
            return false;
        }
        let Some(changed) = body.get(1) else {
            return false;
        };
        Self::DISPLAYED_PROPERTIES
            .iter()
            .any(|property| changed.dict_get(property).is_some())
    }

    /// The first adapter's power/discovery state, or `None` when no
    /// adapter object exists.
    pub fn adapter(conn: &mut DbusConn) -> Result<Option<BluetoothAdapter>, DbusError> {
        let objects = Self::managed_objects(conn)?;
        for entry in objects {
            let Value::DictEntry(path, interfaces) = entry else {
                continue;
            };
            let Some(path) = path.as_display_string() else {
                continue;
            };
            let Some(props) = interfaces.dict_get(ADAPTER_IFACE) else {
                continue;
            };
            return Ok(Some(BluetoothAdapter::from_properties(&path, props)));
        }
        Ok(None)
    }

    /// Every known device: paired ones plus anything a running discovery
    /// has found, sorted connected-first then alphabetically by name.
    pub fn devices(conn: &mut DbusConn) -> Result<Vec<BluetoothDevice>, DbusError> {
        let objects = Self::managed_objects(conn)?;
        let mut devices = Vec::new();
        for entry in objects {
            let Value::DictEntry(path, interfaces) = entry else {
                continue;
            };
            let Some(path) = path.as_display_string() else {
                continue;
            };
            let Some(props) = interfaces.dict_get(DEVICE_IFACE) else {
                continue;
            };
            devices.push(BluetoothDevice::from_properties(&path, props));
        }
        devices.retain(|d| !d.address.is_empty());
        devices.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then_with(|| b.paired.cmp(&a.paired))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(devices)
    }

    /// Power the adapter on or off. A panel opens with this when the
    /// adapter is off (nothing else can happen until it's on).
    pub fn set_powered(conn: &mut DbusConn, adapter_path: &str, on: bool) -> Result<(), DbusError> {
        Self::set_property(
            conn,
            adapter_path,
            ADAPTER_IFACE,
            "Powered",
            Value::Bool(on),
        )
    }

    /// Start (or stop) device discovery. Results accumulate in the
    /// managed-object tree and announce themselves through
    /// [`Self::SIGNAL_RULE`] signals; this call returns no devices.
    pub fn set_discovering(
        conn: &mut DbusConn,
        adapter_path: &str,
        on: bool,
    ) -> Result<(), DbusError> {
        Self::set_property(
            conn,
            adapter_path,
            ADAPTER_IFACE,
            "Discovering",
            Value::Bool(on),
        )
    }

    /// Pair with a device. Blocks until pairing completes or fails —
    /// `JustWorks` devices (headphones, mice) need no user interaction,
    /// and the panel's auto-accept agent answers anything that does.
    pub fn pair(conn: &mut DbusConn, device_path: &str) -> Result<(), DbusError> {
        conn.call(DEST, device_path, DEVICE_IFACE, "Pair", &[])?;
        Ok(())
    }

    /// Connect an already-paired device.
    pub fn connect(conn: &mut DbusConn, device_path: &str) -> Result<(), DbusError> {
        conn.call(DEST, device_path, DEVICE_IFACE, "Connect", &[])?;
        Ok(())
    }

    /// Disconnect a connected device.
    pub fn disconnect(conn: &mut DbusConn, device_path: &str) -> Result<(), DbusError> {
        conn.call(DEST, device_path, DEVICE_IFACE, "Disconnect", &[])?;
        Ok(())
    }

    /// Remove a paired device from bluez's remembered set.
    pub fn remove(
        conn: &mut DbusConn,
        adapter_path: &str,
        device_path: &str,
    ) -> Result<(), DbusError> {
        conn.call(
            DEST,
            adapter_path,
            ADAPTER_IFACE,
            "RemoveDevice",
            &[crate::message::BodyArg::ObjectPath(device_path.to_owned())],
        )?;
        Ok(())
    }

    /// One `GetManagedObjects` round trip: the `a{oa{sa{sv}}}` tree every
    /// other method here walks.
    fn managed_objects(conn: &mut DbusConn) -> Result<Vec<Value>, DbusError> {
        let reply = conn.call(
            DEST,
            ROOT_PATH,
            OBJECT_MANAGER_IFACE,
            "GetManagedObjects",
            &[],
        )?;
        let values = reply.values();
        Ok(values
            .first()
            .and_then(Value::as_items)
            .map_or(Vec::new(), <[Value]>::to_vec))
    }

    /// `org.freedesktop.DBus.Properties.Set` with a variant payload.
    fn set_property(
        conn: &mut DbusConn,
        path: &str,
        iface: &str,
        name: &str,
        value: Value,
    ) -> Result<(), DbusError> {
        conn.call_with_values(
            DEST,
            path,
            PROPERTIES_IFACE,
            "Set",
            &[
                Value::Str(iface.to_owned()),
                Value::Str(name.to_owned()),
                Value::Variant(Box::new(value)),
            ],
            "ssv",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One managed-objects `Device1` entry as it arrives off the wire,
    /// built through the real marshaller so the shape under test is the
    /// wire shape, not a hand-picked approximation.
    fn device_props(paired: bool, connected: bool) -> Value {
        let entry = |key: &str, value: Value| {
            Value::DictEntry(
                Box::new(Value::Str(key.to_owned())),
                Box::new(Value::Variant(Box::new(value))),
            )
        };
        Value::Array(vec![
            entry("Address", Value::Str("AA:BB:CC:DD:EE:FF".into())),
            entry("Name", Value::Str("Headphones".into())),
            entry("Icon", Value::Str("audio-headsets".into())),
            entry("Paired", Value::Bool(paired)),
            entry("Connected", Value::Bool(connected)),
            entry("RSSI", Value::I16(-59)),
        ])
    }

    fn properties_changed(interface: &str, changed: &[(&str, Value)]) -> Reply {
        let entries = changed
            .iter()
            .map(|(key, value)| {
                Value::DictEntry(
                    Box::new(Value::Str((*key).to_owned())),
                    Box::new(Value::Variant(Box::new(value.clone()))),
                )
            })
            .collect();
        let frame = crate::message::MessageBuilder::build_signal_with_values(
            1,
            "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF",
            PROPERTIES_IFACE,
            "PropertiesChanged",
            &[
                Value::Str(interface.to_owned()),
                Value::Array(entries),
                Value::Array(vec![Value::Str("RSSI".into())]),
            ],
            "sa{sv}as",
        )
        .expect("well-formed PropertiesChanged body");
        Reply::parse_message(&frame).expect("built signal parses")
    }

    #[test]
    fn a_displayed_device_property_change_requires_a_refresh() {
        let signal = properties_changed(DEVICE_IFACE, &[("Connected", Value::Bool(true))]);
        assert!(BluetoothPanel::changes_panel_state(&signal));
    }

    #[test]
    fn an_adapter_power_change_requires_a_refresh() {
        let signal = properties_changed(ADAPTER_IFACE, &[("Powered", Value::Bool(false))]);
        assert!(BluetoothPanel::changes_panel_state(&signal));
    }

    #[test]
    fn signal_strength_noise_does_not_require_a_refresh() {
        let signal = properties_changed(DEVICE_IFACE, &[("RSSI", Value::I16(-71))]);
        assert!(!BluetoothPanel::changes_panel_state(&signal));
    }

    #[test]
    fn other_interfaces_do_not_require_a_refresh() {
        let signal = properties_changed(
            "org.bluez.MediaTransport1",
            &[("Connected", Value::Bool(true))],
        );
        assert!(!BluetoothPanel::changes_panel_state(&signal));
    }

    #[test]
    fn objects_appearing_or_vanishing_require_a_refresh() {
        for member in ["InterfacesAdded", "InterfacesRemoved"] {
            let frame = crate::message::MessageBuilder::build_signal(
                1,
                "/",
                OBJECT_MANAGER_IFACE,
                member,
                &[],
            );
            let signal = Reply::parse_message(&frame).expect("built signal parses");
            assert!(BluetoothPanel::changes_panel_state(&signal), "{member}");
        }
    }

    #[test]
    fn device_decode_reads_every_panel_field() {
        let props = device_props(true, false);
        let device =
            BluetoothDevice::from_properties("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", &props);
        assert_eq!(device.address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(device.name, "Headphones");
        assert_eq!(device.icon, "audio-headsets");
        assert!(device.paired);
        assert!(!device.connected);
        assert_eq!(device.path, "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF");
    }

    #[test]
    fn device_decode_defaults_missing_fields_rather_than_failing() {
        // A device discovered seconds ago has no Name yet — the panel
        // shows the address until one arrives.
        let props = Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str("Address".into())),
            Box::new(Value::Variant(Box::new(Value::Str(
                "11:22:33:44:55:66".into(),
            )))),
        )]);
        let device =
            BluetoothDevice::from_properties("/org/bluez/hci0/dev_11_22_33_44_55_66", &props);
        assert_eq!(device.name, "");
        assert!(!device.paired);
        assert!(!device.connected);
    }

    #[test]
    fn adapter_decode_reads_the_three_power_states() {
        let entry = |key: &str, value: Value| {
            Value::DictEntry(
                Box::new(Value::Str(key.to_owned())),
                Box::new(Value::Variant(Box::new(value))),
            )
        };
        let props = Value::Array(vec![
            entry("Powered", Value::Bool(true)),
            entry("Discoverable", Value::Bool(false)),
            entry("Discovering", Value::Bool(true)),
        ]);
        let adapter = BluetoothAdapter::from_properties("/org/bluez/hci0", &props);
        assert!(adapter.powered);
        assert!(!adapter.discoverable);
        assert!(adapter.discovering);
    }
}
