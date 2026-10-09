//! [`MonitorPalette`] -- every colour the dashboard paints with, resolved
//! once from [`config::Config`].

use platform::draw::Color;

pub struct MonitorPalette {
    pub background: Color,
    pub surface: Color,
    pub border: Color,
    pub foreground: Color,
    pub dim: Color,
    pub faint: Color,
    pub cpu: Color,
    pub memory: Color,
    pub temperature: Color,
    pub network: Color,
}

impl MonitorPalette {
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        Self {
            background: cfg.monitor_background().into(),
            surface: cfg.monitor_surface().into(),
            border: cfg.monitor_border().into(),
            foreground: cfg.monitor_foreground().into(),
            dim: cfg.monitor_dim().into(),
            faint: cfg.monitor_faint().into(),
            cpu: cfg.monitor_cpu().into(),
            memory: cfg.monitor_memory().into(),
            temperature: cfg.monitor_temperature().into(),
            network: cfg.monitor_network().into(),
        }
    }

    #[must_use]
    pub const fn builtin() -> Self {
        Self {
            background: Color::rgb(0x0a, 0x0a, 0x0b),
            surface: Color::rgb(0x18, 0x16, 0x16),
            border: Color::rgb(0x1e, 0x1e, 0x21),
            foreground: Color::rgb(0xf2, 0xf2, 0xf3),
            dim: Color::rgb(0x8a, 0x8a, 0x90),
            faint: Color::rgb(0x55, 0x55, 0x5a),
            cpu: Color::rgb(0x8a, 0x9a, 0x7b),
            memory: Color::rgb(0x8e, 0xa4, 0xa2),
            temperature: Color::rgb(0xb6, 0x92, 0x7b),
            network: Color::rgb(0x8e, 0xa4, 0xa2),
        }
    }
}
