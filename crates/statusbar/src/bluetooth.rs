//! Bluetooth power/connection state, via bluez over D-Bus.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh D-Bus round trip, there's no connection to hold open
//! between polls.

/// Bluetooth status queries -- see the module doc for why this is a marker
/// type.
pub struct Bluetooth;

impl Bluetooth {
    /// Whether Bluetooth is powered and how many devices are connected, via
    /// bluez's `ObjectManager.GetManagedObjects` over D-Bus
    /// (`dbus::bluez::BluetoothStatus::query`) instead of shelling out to
    /// `bluetoothctl` and scraping its text output. `None` if the system
    /// bus / bluez aren't reachable, or no adapter exists.
    #[must_use]
    pub fn read() -> Option<String> {
        let mut conn = dbus::DbusConn::system().ok()?;
        let status = dbus::bluez::BluetoothStatus::query(&mut conn).ok()??;
        if !status.powered {
            return Some("󰂲".into());
        }
        if status.connected_count == 0 {
            Some("󰂯".into())
        } else {
            Some(format!("{} 󰂱", status.connected_count))
        }
    }
}
