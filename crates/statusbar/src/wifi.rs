//! Connected Wi-Fi SSID + signal strength, via `NetworkManager` over D-Bus.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh D-Bus round trip, there's no connection to hold open
//! between polls.

/// Wi-Fi status queries -- see the module doc for why this is a marker type.
pub struct Wifi;

impl Wifi {
    /// Connected Wi-Fi SSID + a signal-strength percentage, via
    /// `NetworkManager` over D-Bus (`dbus::network_manager::WifiStatus::query`)
    /// instead of shelling out to `iw` and scraping its text output. `None`
    /// only when the system bus / `NetworkManager` aren't reachable at all; a
    /// Wi-Fi device present but not associated to an access point still
    /// renders the disconnected icon (matches the old `iw`-based "Not
    /// connected" case, not a `None`).
    #[must_use]
    pub fn read() -> Option<String> {
        let mut conn = dbus::DbusConn::system().ok()?;
        match dbus::network_manager::WifiStatus::query(&mut conn).ok()? {
            Some(status) => Some(format!("{} 󰤨", status.ssid)),
            None => Some("󰤭".into()),
        }
    }
}
