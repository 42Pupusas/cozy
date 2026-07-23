//! `org.bluez` convenience calls: adapter power state + connected device
//! count.
//!
//! Replaces shelling out to `bluetoothctl show` / `bluetoothctl devices
//! Connected` and text-scraping the output. bluez exposes its whole object
//! tree (adapters, devices, each as a D-Bus object implementing
//! `org.bluez.Adapter1` / `org.bluez.Device1`) through the standard
//! `org.freedesktop.DBus.ObjectManager.GetManagedObjects` call, which hands
//! back everything in one round trip: `a{oa{sa{sv}}}` — object path ->
//! interface name -> property map.

use crate::conn::{DbusConn, DbusError};
use crate::value::Value;

const DEST: &str = "org.bluez";
const ROOT_PATH: &str = "/";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";

/// Bluetooth status: whether any adapter is powered, and how many devices
/// are currently connected.
///
/// `None` fields mean "no adapter found" (bluez not running, or genuinely no
/// hardware) — the statusbar module already treats a `None` return the same
/// as the old `bluetoothctl`-missing case.
#[derive(Debug, Clone)]
pub struct BluetoothStatus {
    pub powered: bool,
    pub connected_count: u32,
}

impl BluetoothStatus {
    /// Walk bluez's managed-object tree once and summarize adapter power +
    /// connected-device count.
    ///
    /// `Ok(None)` when no adapter object exists at all (distinct from
    /// "adapter present but off", which is `Ok(Some(status))` with
    /// `powered: false`).
    pub fn query(conn: &mut DbusConn) -> Result<Option<Self>, DbusError> {
        let reply = conn.call(
            DEST,
            ROOT_PATH,
            OBJECT_MANAGER_IFACE,
            "GetManagedObjects",
            &[],
        )?;
        let values = reply.values();
        let Some(objects) = values.first().and_then(Value::as_items) else {
            return Ok(None);
        };

        let mut saw_adapter = false;
        let mut powered = false;
        let mut connected_count = 0u32;

        for entry in objects {
            let Value::DictEntry(_path, interfaces) = entry else {
                continue;
            };
            let Some(interfaces) = interfaces.as_items() else {
                continue;
            };
            for iface_entry in interfaces {
                let Value::DictEntry(iface_name, props) = iface_entry else {
                    continue;
                };
                let Some(name) = iface_name.as_display_string() else {
                    continue;
                };
                match name.as_str() {
                    s if s == ADAPTER_IFACE => {
                        saw_adapter = true;
                        if props
                            .dict_get("Powered")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            powered = true;
                        }
                    }
                    s if s == DEVICE_IFACE
                        && props
                            .dict_get("Connected")
                            .and_then(Value::as_bool)
                            .unwrap_or(false) =>
                    {
                        connected_count += 1;
                    }
                    _ => {}
                }
            }
        }

        if !saw_adapter {
            return Ok(None);
        }
        Ok(Some(Self {
            powered,
            connected_count,
        }))
    }
}
