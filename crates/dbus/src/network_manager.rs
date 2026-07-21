//! `org.freedesktop.NetworkManager` convenience calls: the active Wi-Fi
//! connection's SSID and signal strength.
//!
//! Replaces shelling out to `iw dev` / `iw dev <iface> link` — same
//! information (connected SSID + a signal percentage), read as typed D-Bus
//! properties instead of parsed CLI text.

use crate::conn::{DbusConn, DbusError};

const DEST: &str = "org.freedesktop.NetworkManager";
const ROOT_PATH: &str = "/org/freedesktop/NetworkManager";
const IFACE: &str = "org.freedesktop.NetworkManager";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";

/// NM's `NMDeviceType` enum value for a Wi-Fi device.
const DEVICE_TYPE_WIFI: u64 = 2;

/// The connected network's SSID and signal strength, or `None` if no Wi-Fi
/// device is associated to an access point.
///
/// Mirrors `modules::wifi`'s "Not connected" case returning the
/// disconnected icon upstream — this function just reports the data, the
/// statusbar module still decides how to render "no AP".
#[derive(Debug, Clone)]
pub struct WifiStatus {
    pub ssid: String,
    /// 0-100.
    pub strength: u8,
}

impl WifiStatus {
    /// Query every device `NetworkManager` knows about, find the first
    /// Wi-Fi device, and report its active access point (if any).
    ///
    /// Returns `Ok(None)` when there's a Wi-Fi device but it's not
    /// associated (equivalent to `iw`'s "Not connected"), and `Err` only
    /// for real transport/bus failures.
    pub fn query(conn: &mut DbusConn) -> Result<Option<Self>, DbusError> {
        let reply = conn.call(DEST, ROOT_PATH, IFACE, "GetDevices", &[])?;
        let devices = reply.values();
        let Some(paths) = devices.first().and_then(crate::value::Value::as_items) else {
            return Ok(None);
        };

        for path_val in paths {
            let Some(path) = path_val.as_display_string() else {
                continue;
            };
            let device_type = conn
                .get_property(DEST, &path, DEVICE_IFACE, "DeviceType")?
                .as_u64()
                .unwrap_or(0);
            if device_type != DEVICE_TYPE_WIFI {
                continue;
            }

            let ap_path = conn
                .get_property(DEST, &path, WIRELESS_IFACE, "ActiveAccessPoint")?
                .as_display_string()
                .unwrap_or_default();
            // NM represents "no AP" as the root object path "/".
            if ap_path.is_empty() || ap_path == "/" {
                return Ok(None);
            }

            let ssid = conn
                .get_property(DEST, &ap_path, AP_IFACE, "Ssid")?
                .as_display_string()
                .unwrap_or_default();
            let strength = conn
                .get_property(DEST, &ap_path, AP_IFACE, "Strength")?
                .as_u64()
                .unwrap_or(0)
                .min(100) as u8;

            return Ok(Some(Self { ssid, strength }));
        }

        Ok(None)
    }
}
