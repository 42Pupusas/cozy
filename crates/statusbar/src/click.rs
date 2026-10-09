//! Statusbar click command selection and `{x}` expansion.

use crate::colors::ModuleKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickAction {
    pub program: String,
    pub args: Vec<String>,
    pub fallback: Option<(String, Vec<String>)>,
}

impl ClickAction {
    #[must_use]
    pub fn for_module(
        kind: ModuleKind,
        clicks: &config::BarClickConfig,
        x0: i32,
        x1: i32,
    ) -> Option<Self> {
        if let Some(command) = Self::configured(kind, clicks) {
            return Self::parse(command, x0, x1);
        }
        let page = kind.settings_page()?;
        Some(Self {
            program: "cosmic-popup".into(),
            args: vec![page.into()],
            fallback: Some(("cosmic-settings".into(), vec![page.into()])),
        })
    }

    fn configured(kind: ModuleKind, clicks: &config::BarClickConfig) -> Option<&str> {
        match kind {
            ModuleKind::Wifi => clicks.wifi.as_deref(),
            ModuleKind::Bluetooth => clicks.bluetooth.as_deref(),
            ModuleKind::Battery => clicks.battery.as_deref(),
            ModuleKind::Brightness => clicks.brightness.as_deref(),
            ModuleKind::VolOut => clicks.volume_output.as_deref(),
            ModuleKind::VolIn => clicks.volume_input.as_deref(),
            _ => None,
        }
    }

    fn parse(command: &str, x0: i32, x1: i32) -> Option<Self> {
        const LEFT_TOKEN: &str = "{x}";
        const RIGHT_TOKEN: &str = "{x1}";
        let left = x0.to_string();
        let right = x1.to_string();
        let mut words = command.split_whitespace();
        let program = words
            .next()?
            .replace(RIGHT_TOKEN, &right)
            .replace(LEFT_TOKEN, &left);
        let args = words
            .map(|word| word.replace(RIGHT_TOKEN, &right).replace(LEFT_TOKEN, &left))
            .collect();
        Some(Self {
            program,
            args,
            fallback: None,
        })
    }
}

impl ModuleKind {
    #[must_use]
    pub const fn settings_page(self) -> Option<&'static str> {
        match self {
            Self::Wifi => Some("network"),
            Self::Bluetooth => Some("bluetooth"),
            Self::Battery => Some("power"),
            Self::Brightness => Some("displays"),
            Self::VolOut | Self::VolIn => Some("sound"),
            Self::Clock
            | Self::Date
            | Self::Temp
            | Self::Cpu
            | Self::Mem
            | Self::Notification
            | Self::SystemTray => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_panel_expands_position() {
        let clicks = config::BarClickConfig {
            wifi: Some("panel-wifi --at {x} --at-right {x1}".into()),
            ..config::BarClickConfig::default()
        };
        let action = ClickAction::for_module(ModuleKind::Wifi, &clicks, 300, 317).unwrap();
        assert_eq!(action.program, "panel-wifi");
        assert_eq!(action.args, ["--at", "300", "--at-right", "317"]);
        assert!(action.fallback.is_none());
    }

    #[test]
    fn missing_override_keeps_cosmic_default() {
        let action = ClickAction::for_module(
            ModuleKind::Bluetooth,
            &config::BarClickConfig::default(),
            100,
            120,
        )
        .unwrap();
        assert_eq!(action.program, "cosmic-popup");
        assert_eq!(action.args, ["bluetooth"]);
        assert_eq!(
            action.fallback,
            Some(("cosmic-settings".into(), vec!["bluetooth".into()]))
        );
    }

    #[test]
    fn unmapped_module_has_no_action() {
        assert!(
            ClickAction::for_module(ModuleKind::Clock, &config::BarClickConfig::default(), 0, 10)
                .is_none()
        );
    }
}
