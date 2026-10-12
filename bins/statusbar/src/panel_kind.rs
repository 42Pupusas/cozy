//! Which built-in panel a bar module opens.

use panel_bluetooth::BluetoothPopupPanel;
use panel_ui::Panel;
use panel_volume::VolumePanel;
use panel_wifi::WifiPanel;
use statusbar::ModuleKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKind {
    Wifi,
    Bluetooth,
    Volume,
}

impl PanelKind {
    #[must_use]
    pub const fn for_module(kind: ModuleKind) -> Option<Self> {
        match kind {
            ModuleKind::Wifi => Some(Self::Wifi),
            ModuleKind::Bluetooth => Some(Self::Bluetooth),
            ModuleKind::VolOut | ModuleKind::VolIn => Some(Self::Volume),
            ModuleKind::Battery
            | ModuleKind::Brightness
            | ModuleKind::Clock
            | ModuleKind::Date
            | ModuleKind::Temp
            | ModuleKind::Cpu
            | ModuleKind::Mem
            | ModuleKind::Notification
            | ModuleKind::SystemTray => None,
        }
    }

    /// Start the panel's worker; `wake` is signalled on each update.
    pub fn open(self, wake: waker::Waker) -> Box<dyn Panel> {
        match self {
            Self::Wifi => Box::new(WifiPanel::open(wake)),
            Self::Bluetooth => Box::new(BluetoothPopupPanel::open(wake)),
            Self::Volume => Box::new(VolumePanel::open(wake)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_volume_modules_share_one_panel() {
        assert_eq!(
            PanelKind::for_module(ModuleKind::VolOut),
            PanelKind::for_module(ModuleKind::VolIn)
        );
    }

    #[test]
    fn modules_without_a_panel_map_to_none() {
        for kind in [
            ModuleKind::Clock,
            ModuleKind::Battery,
            ModuleKind::SystemTray,
        ] {
            assert_eq!(PanelKind::for_module(kind), None);
        }
    }
}
