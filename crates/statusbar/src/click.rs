//! Left-click → COSMIC settings page dispatch, mirroring the user's own
//! `~/.config/waybar/config` `"on-click": "cosmic-popup <page>"` bindings
//! module-for-module (`cosmic-popup` is their own `~/.local/bin` wrapper
//! around `cosmic-settings <page>` that auto-closes on focus loss — see
//! `cosmic-settings help` for the page slug list). Clock/date/temp have no
//! waybar `on-click` of their own, so they're left unmapped here too.
//!
//! `ModuleKind::settings_page` is a pure lookup (no process spawning here)
//! so the mapping itself is unit-testable without a live desktop session;
//! the caller (`worker::run`) does the actual `Command::spawn`.

use crate::colors::ModuleKind;

impl ModuleKind {
    /// The `cosmic-settings` page slug a click on this module should open,
    /// or `None` if this module has no click action (matches which waybar
    /// modules do/don't set `on-click` in the user's config).
    #[must_use]
    pub fn settings_page(self) -> Option<&'static str> {
        match self {
            Self::Wifi => Some("network"),
            Self::Bluetooth => Some("bluetooth"),
            Self::Battery => Some("power"),
            Self::Brightness => Some("displays"),
            Self::VolOut | Self::VolIn => Some("sound"),
            Self::Clock | Self::Date | Self::Temp | Self::Cpu | Self::Mem | Self::Notification => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_clickable_module_to_waybar_s_own_binding() {
        // Matches ~/.config/waybar/config's "on-click": "cosmic-popup <page>"
        // entries verbatim.
        assert_eq!(ModuleKind::Wifi.settings_page(), Some("network"));
        assert_eq!(ModuleKind::Bluetooth.settings_page(), Some("bluetooth"));
        assert_eq!(ModuleKind::Battery.settings_page(), Some("power"));
        assert_eq!(ModuleKind::Brightness.settings_page(), Some("displays"));
        assert_eq!(ModuleKind::VolOut.settings_page(), Some("sound"));
        assert_eq!(ModuleKind::VolIn.settings_page(), Some("sound"));
    }

    #[test]
    fn modules_without_a_waybar_on_click_are_unmapped() {
        assert_eq!(ModuleKind::Clock.settings_page(), None);
        assert_eq!(ModuleKind::Date.settings_page(), None);
        assert_eq!(ModuleKind::Temp.settings_page(), None);
        assert_eq!(ModuleKind::Cpu.settings_page(), None);
        assert_eq!(ModuleKind::Mem.settings_page(), None);
    }
}
