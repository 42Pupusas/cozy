//! Shared command-line entry plumbing for the three panel binaries.
//!
//! Every panel takes the same arguments (the bar expands them from its
//! click config), loads the same `[panel]` config, and discovers the
//! output's logical width itself when `--screen-w` is absent -- gathered
//! here so the three `main`s stay thin and cannot drift apart.

use crate::placement::ModuleAnchor;
use crate::widgets::Theme;

/// The arguments every panel accepts. CLI values override `[panel]` for
/// ad-hoc testing (`panel-wifi --side left --gap 16`); unset values fall
/// back to config, then to built-in defaults.
#[derive(Debug, Clone, Copy)]
pub struct PanelArgs {
    /// The module's left edge (`--at`) and right edge (`--at-right`);
    /// `bar_height` here is a placeholder replaced by [`Self::resolve`].
    pub anchor: ModuleAnchor,
    pub bar_height: Option<i32>,
    pub screen_w: Option<i32>,
    pub side: Option<config::PanelSide>,
    pub gap: Option<i32>,
}

impl PanelArgs {
    /// Parse `std::env::args`, skipping argv[0]. Unknown flags are
    /// ignored (not fatal) so a bar config can carry extra arguments
    /// for future panels without breaking old ones.
    #[must_use]
    pub fn parse<I: Iterator<Item = String>>(args: I) -> Self {
        let mut x0 = 20;
        let mut x1 = None;
        let mut screen_w = None;
        let mut side = None;
        let mut gap = None;
        let mut bar_height = None;
        let mut vals = args;
        while let Some(arg) = vals.next() {
            let mut value = || -> Option<i32> { vals.next().and_then(|v| v.parse().ok()) };
            match arg.as_str() {
                "--at" => {
                    if let Some(v) = value() {
                        x0 = v;
                    }
                }
                "--at-right" => x1 = value(),
                "--bar-height" => bar_height = value(),
                "--screen-w" => screen_w = value(),
                "--side" => {
                    if let Some(v) = vals.next() {
                        side = match v.as_str() {
                            "left" => Some(config::PanelSide::Left),
                            "right" => Some(config::PanelSide::Right),
                            "auto" => Some(config::PanelSide::Auto),
                            _ => None,
                        };
                    }
                }
                "--gap" => gap = value(),
                _ => {}
            }
        }
        // x1 defaults to x0 so a bare `--at` keeps today's behavior for
        // side=left; `resolve` keeps that only when x1 was never given.
        let anchor = ModuleAnchor {
            x0,
            x1: x1.unwrap_or(x0),
            bar_height: bar_height.unwrap_or(28),
            position: config::BarPosition::default(),
        };
        Self {
            anchor,
            bar_height,
            screen_w,
            side,
            gap,
        }
    }

    /// Merge with `[panel]` config: CLI wins, then config, then defaults.
    #[must_use]
    pub fn resolve(self, cfg: &config::Config) -> ResolvedPanel {
        let side = self.side.unwrap_or_else(|| cfg.panel_side());
        #[allow(clippy::cast_possible_truncation)]
        let gap = self
            .gap
            .unwrap_or_else(|| i32::try_from(cfg.panel_gap()).unwrap_or(8));
        // `--bar-height` absent means "use `[bar].height`"; present means
        // the bar (which knows its real configured height) already told us.
        let bar_height = self
            .bar_height
            .unwrap_or_else(|| i32::try_from(cfg.bar_height()).unwrap_or(28));
        let anchor = ModuleAnchor {
            bar_height,
            position: cfg.bar_position(),
            ..self.anchor
        };
        ResolvedPanel {
            anchor,
            side,
            gap,
            screen_w: self.screen_w,
            theme: Theme::from_config(cfg),
        }
    }
}

/// `PanelArgs` merged with config -- everything `PopupWindow::new` needs.
pub struct ResolvedPanel {
    pub anchor: ModuleAnchor,
    pub side: config::PanelSide,
    pub gap: i32,
    pub screen_w: Option<i32>,
    pub theme: Theme,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_placement_flags() {
        let a = PanelArgs::parse(
            [
                "--at",
                "600",
                "--at-right",
                "680",
                "--bar-height",
                "28",
                "--screen-w",
                "1920",
                "--side",
                "right",
                "--gap",
                "12",
            ]
            .into_iter()
            .map(String::from),
        );
        assert_eq!(a.anchor.x0, 600);
        assert_eq!(a.anchor.x1, 680);
        assert_eq!(a.anchor.bar_height, 28);
        assert_eq!(a.screen_w, Some(1920));
        assert_eq!(a.side, Some(config::PanelSide::Right));
        assert_eq!(a.gap, Some(12));
    }

    #[test]
    fn unparsed_and_unknown_flags_keep_defaults() {
        let a = PanelArgs::parse(
            ["--nonsense", "--at", "abc", "--side", "middle"]
                .into_iter()
                .map(String::from),
        );
        assert_eq!(a.anchor.x0, 20, "unparseable --at keeps the default");
        assert_eq!(a.side, None, "unknown side keeps config's choice");
    }

    #[test]
    fn bar_height_falls_back_to_config() {
        let a = PanelArgs::parse(std::iter::empty());
        let cfg = config::Config::from_bytes(b"[bar]\nheight = 32\n").unwrap();
        let r = a.resolve(&cfg);
        assert_eq!(r.anchor.bar_height, 32, "config bar height used");
        assert_eq!(r.gap, 8, "default gap");
        assert_eq!(r.side, config::PanelSide::Auto);
        assert_eq!(r.anchor.position, config::BarPosition::Top);
    }

    #[test]
    fn bar_position_falls_back_to_config() {
        let a = PanelArgs::parse(std::iter::empty());
        let cfg = config::Config::from_bytes(b"[bar]\nposition = \"bottom\"\n").unwrap();
        assert_eq!(a.resolve(&cfg).anchor.position, config::BarPosition::Bottom);
    }

    #[test]
    fn cli_overrides_win_over_config() {
        let a = PanelArgs::parse(
            ["--gap", "16", "--side", "left"]
                .into_iter()
                .map(String::from),
        );
        let cfg = config::Config::from_bytes(b"[panel]\ngap = 4\nside = \"right\"\n").unwrap();
        let r = a.resolve(&cfg);
        assert_eq!(r.gap, 16);
        assert_eq!(r.side, config::PanelSide::Left);
    }
}
