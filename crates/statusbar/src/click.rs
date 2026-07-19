//! Left-click → COSMIC settings page dispatch, mirroring the user's own
//! `~/.config/waybar/config` `"on-click": "cosmic-popup <page>"` bindings
//! module-for-module (`cosmic-popup` is their own `~/.local/bin` wrapper
//! around `cosmic-settings <page>` that auto-closes on focus loss — see
//! `cosmic-settings help` for the page slug list). Clock/date/temp have no
//! waybar `on-click` of their own, so they're left unmapped here too.
//!
//! Kept as a pure `ModuleKind -> Option<&'static str>` lookup (no process
//! spawning here) so the mapping itself is unit-testable without a live
//! desktop session; the caller (`worker::run`) does the actual
//! `Command::spawn`.

use crate::colors::ModuleKind;

/// The `cosmic-settings` page slug a click on this module should open, or
/// `None` if this module has no click action (matches which waybar modules
/// do/don't set `on-click` in the user's config).
#[must_use]
pub fn settings_page(kind: ModuleKind) -> Option<&'static str> {
    match kind {
        ModuleKind::Wifi => Some("network"),
        ModuleKind::Bluetooth => Some("bluetooth"),
        ModuleKind::Battery => Some("power"),
        ModuleKind::Brightness => Some("displays"),
        ModuleKind::VolOut | ModuleKind::VolIn => Some("sound"),
        ModuleKind::Clock
        | ModuleKind::Date
        | ModuleKind::Temp
        | ModuleKind::Cpu
        | ModuleKind::Mem => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_clickable_module_to_waybar_s_own_binding() {
        // Matches ~/.config/waybar/config's "on-click": "cosmic-popup <page>"
        // entries verbatim.
        assert_eq!(settings_page(ModuleKind::Wifi), Some("network"));
        assert_eq!(settings_page(ModuleKind::Bluetooth), Some("bluetooth"));
        assert_eq!(settings_page(ModuleKind::Battery), Some("power"));
        assert_eq!(settings_page(ModuleKind::Brightness), Some("displays"));
        assert_eq!(settings_page(ModuleKind::VolOut), Some("sound"));
        assert_eq!(settings_page(ModuleKind::VolIn), Some("sound"));
    }

    #[test]
    fn modules_without_a_waybar_on_click_are_unmapped() {
        assert_eq!(settings_page(ModuleKind::Clock), None);
        assert_eq!(settings_page(ModuleKind::Date), None);
        assert_eq!(settings_page(ModuleKind::Temp), None);
        assert_eq!(settings_page(ModuleKind::Cpu), None);
        assert_eq!(settings_page(ModuleKind::Mem), None);
    }
}
