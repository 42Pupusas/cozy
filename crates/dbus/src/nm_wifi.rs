//! `org.freedesktop.NetworkManager` Wi-Fi panel surface.
//!
//! Device discovery, access-point scans, saved connection profiles, and
//! activate/deactivate — everything a "scan and connect" popup needs,
//! beyond what the always-on bar module's [`crate::network_manager`]
//! (connected SSID + strength) reads.
//!
//! Everything here is a plain blocking round trip over a
//! [`DbusConn`](crate::conn::DbusConn) owned by the caller, matching the
//! crate's no-event-loop contract: a panel worker thread opens one
//! connection, drives it with these calls, and re-scans on demand.

use crate::conn::{DbusConn, DbusError};
use crate::message::BodyArg;
use crate::value::Value;

const DEST: &str = "org.freedesktop.NetworkManager";
const ROOT_PATH: &str = "/org/freedesktop/NetworkManager";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const IFACE: &str = "org.freedesktop.NetworkManager";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const DEVICE_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";

/// NM's `NMDeviceType` enum value for a Wi-Fi device.
const DEVICE_TYPE_WIFI: u64 = 2;

/// One access point from a scan: SSID (lossily decoded from NM's raw `ay`
/// bytes — SSIDs are arbitrary bytes, not guaranteed UTF-8), signal
/// strength, and whether it asks for a password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    pub ssid: String,
    /// 0-100, straight from NM's `Strength` property.
    pub strength: u8,
    /// `true` when the AP carries privacy flags, i.e. connecting needs
    /// credentials from the panel's user.
    pub secured: bool,
    /// NM object path of the AP object — the token [`NmWifi::connect`]
    /// keys its request on, distinct from the display SSID.
    pub path: String,
}

/// One saved connection profile from NM's settings service: its id, and
/// the Wi-Fi SSID it belongs to (empty when the profile is not Wi-Fi at
/// all — [`NmWifi::saved_connections`] filters those out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConnection {
    pub id: String,
    pub ssid: String,
    /// NM object path of the settings connection — the token
    /// [`NmWifi::connect`] (reusing a saved profile) and
    /// [`NmWifi::forget`] key their requests on.
    pub path: String,
}

/// The machine's Wi-Fi device (or `None` on machines without one) — the
/// top of the panel's model tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiDevice {
    pub path: String,
    pub interface: String,
}

/// Everything a Wi-Fi panel shows in one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WifiPanelSnapshot {
    pub device: Option<WifiDevice>,
    pub access_points: Vec<AccessPoint>,
    pub saved: Vec<SavedConnection>,
    /// `None` = not connected; otherwise NM's `NM_ACTIVE_CONNECTION_STATE`
    /// (`2` = activated) and the connection's id.
    pub active: Option<(u64, String)>,
}

/// Marker type owning the panel-facing `NetworkManager` calls.
///
/// Same shape as [`crate::network_manager::WifiStatus`]: no connection
/// state of its own, the caller owns the [`DbusConn`](crate::conn::DbusConn).
pub struct NmWifi;

impl NmWifi {
    /// The match rule that delivers every signal `NetworkManager` emits.
    ///
    /// Property changes, access points appearing and vanishing, active
    /// connection state transitions and profile changes all arrive as
    /// signals from this one sender, so a panel needs no timer to notice
    /// any of them.
    pub const SIGNAL_RULE: &'static str = "type='signal',sender='org.freedesktop.NetworkManager'";

    /// Subscribe `conn` to everything `NetworkManager` announces.
    pub fn subscribe(conn: &mut DbusConn) -> Result<(), DbusError> {
        conn.add_match(Self::SIGNAL_RULE)
    }

    /// The first managed Wi-Fi device's object path + interface name, or
    /// `None` on a machine with no Wi-Fi hardware (or NM not running).
    pub fn device(conn: &mut DbusConn) -> Result<Option<WifiDevice>, DbusError> {
        let reply = conn.call(DEST, ROOT_PATH, IFACE, "GetDevices", &[])?;
        let devices = reply.values();
        let Some(paths) = devices.first().and_then(Value::as_items) else {
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
            let interface = conn
                .get_property(DEST, &path, DEVICE_IFACE, "Interface")?
                .as_display_string()
                .unwrap_or_default();
            return Ok(Some(WifiDevice { path, interface }));
        }
        Ok(None)
    }

    /// Ask the Wi-Fi device to rescan. The `RequestScan` call returns when
    /// the scan *starts*; results land via the device's access-point list
    /// once finished; the panel re-reads
    /// [`Self::access_points`] when `NetworkManager`'s signals announce
    /// them, not from this call.
    pub fn scan(conn: &mut DbusConn, device_path: &str) -> Result<(), DbusError> {
        conn.call_with_values(
            DEST,
            device_path,
            WIRELESS_IFACE,
            "RequestScan",
            &[Value::Array(Vec::new())],
            "a{sv}",
        )?;
        Ok(())
    }

    /// The device's current access-point list (`GetAllAccessPoints` —
    /// every AP, not just the associated one), sorted strongest-first.
    pub fn access_points(
        conn: &mut DbusConn,
        device_path: &str,
    ) -> Result<Vec<AccessPoint>, DbusError> {
        let reply = conn.call(DEST, device_path, WIRELESS_IFACE, "GetAllAccessPoints", &[])?;
        let ap_paths = reply.values();
        let Some(paths) = ap_paths.first().and_then(Value::as_items) else {
            return Ok(Vec::new());
        };
        let mut aps = Vec::new();
        for path_val in paths {
            let Some(path) = path_val.as_display_string() else {
                continue;
            };
            if let Some(ap) = Self::read_access_point(conn, &path)? {
                aps.push(ap);
            }
        }
        aps.sort_by(|a, b| {
            b.strength
                .cmp(&a.strength)
                .then_with(|| a.ssid.cmp(&b.ssid))
        });
        Ok(aps)
    }

    /// Decode one AP object's properties into an [`AccessPoint`], or
    /// `None` when the object vanished mid-walk (races a scan).
    fn read_access_point(
        conn: &mut DbusConn,
        path: &str,
    ) -> Result<Option<AccessPoint>, DbusError> {
        let props = conn.get_all_properties(DEST, path, AP_IFACE)?;
        let Some(entries) = props.as_items() else {
            return Ok(None);
        };
        let mut ssid = String::new();
        let mut strength = 0u8;
        let mut privacy = false;
        let mut wpa_flags = 0u64;
        let mut rsn_flags = 0u64;
        for entry in entries {
            let Value::DictEntry(key, value) = entry else {
                continue;
            };
            match key.as_display_string().as_deref() {
                Some("Ssid") => ssid = value.as_display_string().unwrap_or_default(),
                Some("Strength") => strength = value.as_u64().unwrap_or(0).min(100) as u8,
                Some("Flags") => privacy = value.as_u64().unwrap_or(0) & 0x1 != 0,
                Some("WpaFlags") => wpa_flags = value.as_u64().unwrap_or(0),
                Some("RsnFlags") => rsn_flags = value.as_u64().unwrap_or(0),
                _ => {}
            }
        }
        if ssid.is_empty() && strength == 0 {
            return Ok(None);
        }
        Ok(Some(AccessPoint {
            secured: privacy || wpa_flags != 0 || rsn_flags != 0,
            ssid,
            strength,
            path: path.to_owned(),
        }))
    }

    /// Every saved connection profile that is a Wi-Fi profile.
    pub fn saved_connections(conn: &mut DbusConn) -> Result<Vec<SavedConnection>, DbusError> {
        let reply = conn.call(DEST, SETTINGS_PATH, SETTINGS_IFACE, "ListConnections", &[])?;
        let connection_paths = reply.values();
        let Some(paths) = connection_paths.first().and_then(Value::as_items) else {
            return Ok(Vec::new());
        };
        let mut saved = Vec::new();
        for path_val in paths {
            let Some(path) = path_val.as_display_string() else {
                continue;
            };
            let reply = conn.call(DEST, &path, CONNECTION_IFACE, "GetSettings", &[])?;
            let reply_values = reply.values();
            let Some(settings) = reply_values.first().and_then(Value::as_items) else {
                continue;
            };
            if !settings.iter().any(Self::is_wireless_section) {
                continue;
            }
            saved.push(Self::saved_connection_from_settings(&path, settings));
        }
        Ok(saved)
    }

    /// Whether one `a{sa{sv}}` entry is the `802-11-wireless` section.
    fn is_wireless_section(section: &Value) -> bool {
        let Value::DictEntry(name, _) = section else {
            return false;
        };
        name.as_display_string().as_deref() == Some("802-11-wireless")
    }

    /// Pull `(connection.id, 802-11-wireless.ssid)` out of a settings
    /// dict — both live in different sections of the same `a{sa{sv}}`.
    fn saved_connection_from_settings(path: &str, settings: &[Value]) -> SavedConnection {
        let mut id = String::new();
        let mut ssid = String::new();
        for section in settings {
            let Value::DictEntry(name, fields) = section else {
                continue;
            };
            match name.as_display_string().as_deref() {
                Some("connection") => {
                    id = fields
                        .dict_get("id")
                        .and_then(|v| v.clone().into_inner().as_display_string())
                        .unwrap_or_default();
                }
                Some("802-11-wireless") => {
                    ssid = fields
                        .dict_get("ssid")
                        .and_then(|v| v.clone().into_inner().as_display_string())
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
        SavedConnection {
            id,
            ssid,
            path: path.to_owned(),
        }
    }

    /// The active connection (state + id), or `None` when nothing is
    /// active. Wireless-only: a wired or VPN active connection is not the
    /// Wi-Fi panel's business.
    pub fn active_connection(
        conn: &mut DbusConn,
        device_path: &str,
    ) -> Result<Option<(u64, String)>, DbusError> {
        let active = conn
            .get_property(DEST, device_path, WIRELESS_IFACE, "ActiveAccessPoint")?
            .as_display_string()
            .unwrap_or_default();
        if active.is_empty() || active == "/" {
            return Ok(None);
        }
        let paths = conn
            .get_property(DEST, ROOT_PATH, IFACE, "ActiveConnections")?
            .as_items()
            .map_or(&[][..], <[Value]>::as_ref)
            .to_vec();
        for path_val in paths {
            let Some(path) = path_val.as_display_string() else {
                continue;
            };
            let state = conn
                .get_property(DEST, &path, ACTIVE_IFACE, "State")?
                .as_u64()
                .unwrap_or(0);
            let id = conn
                .get_property(DEST, &path, ACTIVE_IFACE, "Id")?
                .as_display_string()
                .unwrap_or_default();
            return Ok(Some((state, id)));
        }
        Ok(None)
    }

    /// Ask NM to activate a saved profile without a current access-point
    /// object, and return once NM has accepted the request.
    ///
    /// Activation proceeds inside NM; its progress is the `State` of the
    /// active connection, which [`Self::active_connection`] reports.
    pub fn activate_saved(
        conn: &mut DbusConn,
        device: &WifiDevice,
        profile_path: &str,
    ) -> Result<(), DbusError> {
        let reply = conn.call(
            DEST,
            ROOT_PATH,
            IFACE,
            "ActivateConnection",
            &[
                BodyArg::ObjectPath(profile_path.to_owned()),
                BodyArg::ObjectPath(device.path.clone()),
                BodyArg::ObjectPath("/".into()),
            ],
        )?;
        Self::first_object_path(&reply).map_or(Err(DbusError::Malformed), |_| Ok(()))
    }

    /// Connect `device` to the access point `ap`, reusing a saved profile
    /// with the same SSID when one exists and creating one otherwise.
    /// `password` seeds the new profile's PSK (only used when no saved
    /// profile matches).
    ///
    /// Returns once NM has accepted the request. Activation proceeds inside
    /// NM; its progress is the `State` of the active connection, which
    /// [`Self::active_connection`] reports.
    pub fn connect(
        conn: &mut DbusConn,
        device: &WifiDevice,
        ap: &AccessPoint,
        password: Option<&str>,
    ) -> Result<(), DbusError> {
        let saved = Self::saved_connections(conn)?;
        let existing = saved.iter().find(|s| s.ssid == ap.ssid);
        let accepted = if let Some(profile) = existing {
            let reply = conn.call(
                DEST,
                ROOT_PATH,
                IFACE,
                "ActivateConnection",
                &[
                    BodyArg::ObjectPath(profile.path.clone()),
                    BodyArg::ObjectPath(device.path.clone()),
                    BodyArg::ObjectPath(ap.path.clone()),
                ],
            )?;
            Self::first_object_path(&reply)
        } else {
            let settings = Self::settings_for(&ap.ssid, password);
            let reply = conn.call_with_values(
                DEST,
                ROOT_PATH,
                IFACE,
                "AddAndActivateConnection",
                &[
                    settings,
                    Value::ObjectPath(device.path.clone()),
                    Value::ObjectPath(ap.path.clone()),
                ],
                "a{sa{sv}}oo",
            )?;
            Self::first_object_path(&reply)
        };
        accepted.map_or(Err(DbusError::Malformed), |_| Ok(()))
    }

    /// Deactivate whatever connection is active on `device`.
    pub fn disconnect(conn: &mut DbusConn, device_path: &str) -> Result<(), DbusError> {
        let paths = conn
            .get_property(DEST, ROOT_PATH, IFACE, "ActiveConnections")?
            .as_items()
            .map_or(&[][..], <[Value]>::as_ref)
            .to_vec();
        for path_val in paths {
            let Some(path) = path_val.as_display_string() else {
                continue;
            };
            let devices = conn
                .get_property(DEST, &path, ACTIVE_IFACE, "Devices")?
                .as_items()
                .map_or(&[][..], <[Value]>::as_ref)
                .to_vec();
            let ours = devices
                .iter()
                .any(|d| d.as_display_string().as_deref() == Some(device_path));
            if ours {
                conn.call(
                    DEST,
                    ROOT_PATH,
                    IFACE,
                    "DeactivateConnection",
                    &[BodyArg::ObjectPath(path)],
                )?;
                return Ok(());
            }
        }
        Ok(())
    }

    /// Delete a saved profile so it never auto-connects again.
    pub fn forget(conn: &mut DbusConn, saved_path: &str) -> Result<(), DbusError> {
        conn.call(DEST, saved_path, CONNECTION_IFACE, "Delete", &[])?;
        Ok(())
    }

    /// First object path out of a reply body (`(oo)` or `(o)` tuples).
    fn first_object_path(reply: &crate::message::Reply) -> Option<String> {
        reply
            .values()
            .into_iter()
            .find_map(|v| match v.into_inner() {
                Value::ObjectPath(p) => Some(p),
                _ => None,
            })
    }

    /// Build the `a{sa{sv}}` connection-settings dict NM's
    /// `AddAndActivateConnection` takes: a `connection` section (id/type/
    /// autoconnect), an `802-11-wireless` section (raw-byte SSID,
    /// infrastructure mode), and — only with a password — an
    /// `802-11-wireless-security` section with a WPA-PSK key.
    fn settings_for(ssid: &str, password: Option<&str>) -> Value {
        let byte_array = Value::Array(ssid.bytes().map(Value::Byte).collect());
        let mut sections: Vec<Value> = vec![
            Value::DictEntry(
                Box::new(Value::Str("connection".into())),
                Box::new(Value::Array(vec![
                    variant_entry("id", Value::Str(ssid.into())),
                    variant_entry("type", Value::Str("802-11-wireless".into())),
                    variant_entry("autoconnect", Value::Bool(true)),
                ])),
            ),
            Value::DictEntry(
                Box::new(Value::Str("802-11-wireless".into())),
                Box::new(Value::Array(vec![
                    variant_entry("ssid", byte_array),
                    variant_entry("mode", Value::Str("infrastructure".into())),
                ])),
            ),
        ];
        if let Some(psk) = password {
            sections.push(Value::DictEntry(
                Box::new(Value::Str("802-11-wireless-security".into())),
                Box::new(Value::Array(vec![
                    variant_entry("key-mgmt", Value::Str("wpa-psk".into())),
                    variant_entry("psk", Value::Str(psk.into())),
                ])),
            ));
        }
        Value::Array(sections)
    }
}

/// One `"key": variant(value)` entry inside an `a{sv}` section.
fn variant_entry(key: &str, value: Value) -> Value {
    Value::DictEntry(
        Box::new(Value::Str(key.into())),
        Box::new(Value::Variant(Box::new(value))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marshal::Marshaller;
    use crate::value::Unmarshaller;

    /// Marshal then unmarshal a settings dict and confirm the round trip
    /// preserves the section names, field names, and values a connection
    /// needs — the wire-format proof that `settings_for` builds what NM's
    /// `a{sa{sv}}` grammar expects.
    #[test]
    fn settings_dict_round_trips_through_the_wire_format() {
        let settings = NmWifi::settings_for("home", Some("hunter2"));

        let mut wire = Vec::new();
        assert!(Marshaller::marshal(&mut wire, &settings, "a{sa{sv}}").is_some());

        let (decoded, consumed) =
            Unmarshaller::unmarshal(&wire, 0, "a{sa{sv}}").expect("decode marshalled settings");
        assert_eq!(consumed, wire.len());

        let sections = decoded.as_items().expect("dict entries");
        let names: Vec<String> = sections
            .iter()
            .map(|s| {
                let Value::DictEntry(name, _) = s else {
                    panic!("section is not a dict entry");
                };
                name.as_display_string().unwrap_or_default()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "connection".to_owned(),
                "802-11-wireless".to_owned(),
                "802-11-wireless-security".to_owned()
            ]
        );

        let wireless = sections
            .iter()
            .find(|s| {
                let Value::DictEntry(name, _) = s else {
                    return false;
                };
                name.as_display_string().as_deref() == Some("802-11-wireless")
            })
            .and_then(|s| {
                let Value::DictEntry(_, fields) = s else {
                    return None;
                };
                Some(fields.clone())
            })
            .expect("wireless section");
        let ssid = wireless
            .dict_get("ssid")
            .and_then(|v| v.clone().into_inner().as_display_string())
            .expect("ssid field");
        assert_eq!(ssid, "home");

        let security = sections
            .iter()
            .find(|s| {
                let Value::DictEntry(name, _) = s else {
                    return false;
                };
                name.as_display_string().as_deref() == Some("802-11-wireless-security")
            })
            .and_then(|s| {
                let Value::DictEntry(_, fields) = s else {
                    return None;
                };
                Some(fields.clone())
            })
            .expect("security section");
        let psk = security
            .dict_get("psk")
            .and_then(|v| v.clone().into_inner().as_display_string())
            .expect("psk field");
        assert_eq!(psk, "hunter2");
        let key_mgmt = security
            .dict_get("key-mgmt")
            .and_then(|v| v.clone().into_inner().as_display_string())
            .expect("key-mgmt field");
        assert_eq!(key_mgmt, "wpa-psk");
    }

    /// Without a password the security section must be absent — sending an
    /// empty PSK makes NM reject the whole profile.
    #[test]
    fn settings_dict_omits_security_section_without_a_password() {
        let settings = NmWifi::settings_for("open-net", None);
        let Value::Array(sections) = &settings else {
            panic!("not an array");
        };
        let has_security = sections.iter().any(|s| {
            let Value::DictEntry(name, _) = s else {
                return false;
            };
            name.as_display_string().as_deref() == Some("802-11-wireless-security")
        });
        assert!(!has_security);
    }

    /// The empty options dict `RequestScan` takes must marshal under its
    /// explicit `a{sv}` signature — an empty array has no element to infer
    /// a type from, so this is exactly the case `Marshaller`'s explicit
    /// signature rule exists for.
    #[test]
    fn empty_scan_options_marshal_under_explicit_signature() {
        let mut wire = Vec::new();
        let empty = Value::Array(Vec::new());
        assert!(Marshaller::marshal(&mut wire, &empty, "a{sv}").is_some());
        let (decoded, consumed) =
            Unmarshaller::unmarshal(&wire, 0, "a{sv}").expect("decode empty options");
        assert_eq!(consumed, wire.len());
        assert!(decoded.as_items().expect("array").is_empty());
    }
}
