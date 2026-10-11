//! `[bar.click]` command selection and `{x}` / `{x1}` expansion.

use crate::colors::ModuleKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickAction {
    pub program: String,
    pub args: Vec<String>,
}

impl ClickAction {
    /// The command configured for `kind`, with its placeholders expanded.
    #[must_use]
    pub fn for_module(
        kind: ModuleKind,
        clicks: &config::BarClickConfig,
        x0: i32,
        x1: i32,
    ) -> Option<Self> {
        Self::parse(Self::configured(kind, clicks)?, x0, x1)
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
        Some(Self { program, args })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_command_expands_position() {
        let clicks = config::BarClickConfig {
            wifi: Some("nm-applet --at {x} --at-right {x1}".into()),
            ..config::BarClickConfig::default()
        };
        let action = ClickAction::for_module(ModuleKind::Wifi, &clicks, 300, 317).unwrap();
        assert_eq!(action.program, "nm-applet");
        assert_eq!(action.args, ["--at", "300", "--at-right", "317"]);
    }

    #[test]
    fn an_unconfigured_module_has_no_command() {
        let defaults = config::BarClickConfig::default();
        for kind in [
            ModuleKind::Wifi,
            ModuleKind::Bluetooth,
            ModuleKind::VolOut,
            ModuleKind::Battery,
            ModuleKind::Clock,
        ] {
            assert!(ClickAction::for_module(kind, &defaults, 0, 10).is_none());
        }
    }

    #[test]
    fn configured_command_covers_module_without_panel() {
        let clicks = config::BarClickConfig {
            battery: Some("powerprofiles --at {x}".into()),
            ..config::BarClickConfig::default()
        };
        let action = ClickAction::for_module(ModuleKind::Battery, &clicks, 40, 60).unwrap();
        assert_eq!(action.program, "powerprofiles");
        assert_eq!(action.args, ["--at", "40"]);
    }

    #[test]
    fn an_empty_command_is_no_command() {
        let clicks = config::BarClickConfig {
            wifi: Some("   ".into()),
            ..config::BarClickConfig::default()
        };
        assert!(ClickAction::for_module(ModuleKind::Wifi, &clicks, 0, 0).is_none());
    }
}
