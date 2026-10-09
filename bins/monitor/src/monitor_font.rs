//! [`MonitorFont`] -- the one place the dashboard's font is resolved from the
//! config, so the window renderer and the tile text always agree.

use config::{Config, FontConfig};

pub struct MonitorFont {
    font: FontConfig,
    multipliers: [f32; 4],
}

impl MonitorFont {
    #[must_use]
    pub fn from_config(cfg: &Config) -> Self {
        let mut font = cfg.font.clone();
        if let Some(path) = cfg.monitor.font_path.as_ref() {
            font.path = Some(path.clone());
        }
        if let Some(size) = cfg.monitor.font_size {
            font.size = size;
        }
        Self {
            font,
            multipliers: [
                cfg.monitor.hero_size.unwrap_or(2.5),
                cfg.monitor.title_size.unwrap_or(1.0),
                cfg.monitor.subtitle_size.unwrap_or(0.90),
                cfg.monitor.normal_size.unwrap_or(0.80),
            ],
        }
    }

    #[must_use]
    pub const fn font(&self) -> &FontConfig {
        &self.font
    }

    /// Pixel sizes of the hero, title, subtitle and normal tiers at `scale`.
    #[must_use]
    pub fn sizes(&self, scale: f32) -> [f32; 4] {
        let base = self.font.px_size() * scale;
        self.multipliers.map(|multiplier| base * multiplier)
    }
}

#[cfg(test)]
mod tests {
    use super::MonitorFont;

    fn config(toml: &str) -> config::Config {
        let dir = std::env::temp_dir().join(format!("monitor-font-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.toml", toml.len()));
        std::fs::write(&path, toml).unwrap();
        config::Config::load_from(path).unwrap()
    }

    #[test]
    fn the_terminal_font_is_the_default() {
        let font = MonitorFont::from_config(&config("[font]\npath = \"/t.ttf\"\nsize = 14\n"));
        assert_eq!(font.font().path.as_deref(), Some("/t.ttf"));
        assert_eq!(font.font().size, 14);
    }

    #[test]
    fn monitor_overrides_win() {
        let font = MonitorFont::from_config(&config(
            "[font]\npath = \"/t.ttf\"\nsize = 14\n[monitor]\nfont_path = \"/m.ttf\"\nfont_size = 10\n",
        ));
        assert_eq!(font.font().path.as_deref(), Some("/m.ttf"));
        assert_eq!(font.font().size, 10);
    }

    #[test]
    fn tier_sizes_follow_the_scale_and_multipliers() {
        let font = MonitorFont::from_config(&config("[font]\nsize = 12\n"));
        let one = font.sizes(1.0);
        let two = font.sizes(2.0);
        assert!((one[0] / one[1] - 2.5).abs() < 1e-3);
        assert!((two[3] / one[3] - 2.0).abs() < 1e-3);
    }
}
