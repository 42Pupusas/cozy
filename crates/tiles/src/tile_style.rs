use crate::spacing::Spacing;
use crate::theme::Theme;
use crate::typography::Typography;

pub struct TileStyle {
    pub theme: Theme,
    pub type_scale: Typography,
}

impl TileStyle {
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        let type_scale = Typography::load(cfg, 1.0);
        let theme = Theme::from_config(cfg)
            .with_spacing(Spacing::new(cfg.font.px_size(), 1.0))
            .with_metrics(
                type_scale.natural_row_height(),
                type_scale.natural_header_height(),
                type_scale.natural_status_height(),
            );
        Self { theme, type_scale }
    }
}

#[cfg(test)]
mod tests {
    use super::TileStyle;

    #[test]
    fn no_band_is_shorter_than_the_glyphs_it_holds() {
        let style = TileStyle::from_config(&config::Config::default());
        assert!(style.theme.row_height >= style.type_scale.natural_row_height());
        assert!(style.theme.header_height >= style.type_scale.natural_header_height());
        assert!(style.theme.status_height >= style.type_scale.natural_status_height());
    }

    #[test]
    fn a_larger_font_grows_the_spacing() {
        let mut large = config::Config::default();
        large.font.size *= 2;
        let small = TileStyle::from_config(&config::Config::default());
        let large = TileStyle::from_config(&large);
        assert!(large.theme.spacing.m() > small.theme.spacing.m());
    }
}
