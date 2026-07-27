//! Swaybg-compatible appearance rules and the CLI that builds them.
//!
//! A rule is selected by its `wl_output.name`; `*` is the fallback rule. The
//! parser preserves option order so `-o` changes the destination for following
//! appearance options, exactly like swaybg.

use std::path::PathBuf;

use wallpaper::ScaleMode;

/// Opaque RGB color used behind transparent pixels and as a solid wallpaper.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BackgroundColor([u8; 3]);

impl BackgroundColor {
    /// Parse swaybg's `rrggbb` or `#rrggbb` spelling.
    pub fn parse(value: &str) -> Result<Self, String> {
        let hex = value.strip_prefix('#').unwrap_or(value);
        if hex.len() != 6 {
            return Err(format!("color {value:?} must be six hexadecimal digits"));
        }
        let red = u8::from_str_radix(&hex[0..2], 16)
            .map_err(|_| format!("color {value:?} must be hexadecimal"))?;
        let green = u8::from_str_radix(&hex[2..4], 16)
            .map_err(|_| format!("color {value:?} must be hexadecimal"))?;
        let blue = u8::from_str_radix(&hex[4..6], 16)
            .map_err(|_| format!("color {value:?} must be hexadecimal"))?;
        Ok(Self([red, green, blue]))
    }

    /// Pack as native-endian XRGB8888 for the shared-memory surface.
    #[must_use]
    pub const fn xrgb(self) -> u32 {
        ((self.0[0] as u32) << 16) | ((self.0[1] as u32) << 8) | self.0[2] as u32
    }

    /// RGBA padding color passed to the headless scale renderer.
    #[must_use]
    pub const fn rgba(self) -> [u8; 4] {
        [self.0[0], self.0[1], self.0[2], 255]
    }
}

/// Image or solid-color content for one output rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackgroundSource {
    Image(PathBuf),
    SolidColor,
}

/// The accumulated appearance options for an output selector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceRule {
    selector: String,
    source: Option<BackgroundSource>,
    mode: ScaleMode,
    color: BackgroundColor,
}

impl AppearanceRule {
    const fn new(selector: String) -> Self {
        Self {
            selector,
            source: None,
            mode: ScaleMode::Stretch,
            color: BackgroundColor([0, 0, 0]),
        }
    }

    /// Resolve this rule to a complete appearance; a missing image is a solid
    /// color, which also makes `-c` alone a useful invocation.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        Appearance {
            source: self.source.clone().unwrap_or(BackgroundSource::SolidColor),
            mode: self.mode,
            color: self.color,
        }
    }
}

/// Fully specified rendering input selected for a named output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appearance {
    pub source: BackgroundSource,
    pub mode: ScaleMode,
    pub color: BackgroundColor,
}

/// Ordered, per-output rules constructed from swaybg-style arguments.
pub struct AppearanceRules {
    rules: Vec<AppearanceRule>,
}

impl AppearanceRules {
    /// Parse `swaybg`-compatible option syntax.
    pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let program = args.next().unwrap_or_default();
        let mut rules = vec![AppearanceRule::new("*".to_string())];
        let mut selected = 0usize;
        while let Some(argument) = args.next() {
            let argument = argument.to_string_lossy();
            match argument.as_ref() {
                "-h" | "--help" => return Err(Self::usage(&program)),
                "-v" | "--version" => return Err("wallpaper 0.1.0".to_string()),
                "-o" | "--output" => {
                    let selector = Self::required_value(&mut args, &argument)?;
                    rules.push(AppearanceRule::new(selector));
                    selected = rules.len() - 1;
                }
                "-i" | "--image" => {
                    let path = Self::required_value(&mut args, &argument)?;
                    rules[selected].source = Some(BackgroundSource::Image(path.into()));
                }
                "-m" | "--mode" => {
                    let mode = Self::required_value(&mut args, &argument)?;
                    rules[selected].mode = Self::parse_mode(&mode)?;
                    if mode == "solid_color" {
                        rules[selected].source = Some(BackgroundSource::SolidColor);
                    }
                }
                "-c" | "--color" => {
                    let color = Self::required_value(&mut args, &argument)?;
                    rules[selected].color = BackgroundColor::parse(&color)?;
                }
                _ => {
                    return Err(format!(
                        "unknown option {argument:?}\n{}",
                        Self::usage(&program)
                    ));
                }
            }
        }
        Ok(Self { rules })
    }

    /// Select the last matching named rule, otherwise the last `*` rule.
    /// Later rules override earlier rules just as later swaybg options do.
    #[must_use]
    pub fn for_output(&self, name: &str) -> Appearance {
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.selector == name)
            .or_else(|| self.rules.iter().rev().find(|rule| rule.selector == "*"))
            .expect("the parser always creates a wildcard rule")
            .appearance()
    }

    fn required_value(
        args: &mut impl Iterator<Item = std::ffi::OsString>,
        option: &str,
    ) -> Result<String, String> {
        args.next()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| format!("option {option} needs a value"))
    }

    fn parse_mode(value: &str) -> Result<ScaleMode, String> {
        match value {
            "stretch" | "solid_color" => Ok(ScaleMode::Stretch),
            "fit" => Ok(ScaleMode::Fit),
            "fill" => Ok(ScaleMode::Fill),
            "center" => Ok(ScaleMode::Center),
            "tile" => Ok(ScaleMode::Tile),
            _ => Err(format!(
                "unknown mode {value:?}; expected stretch, fill, fit, center, tile, or solid_color"
            )),
        }
    }

    /// Render the command synopsis shared by normal parser errors and `--help`.
    #[must_use]
    pub fn usage(program: &std::ffi::OsStr) -> String {
        format!(
            "usage: {} [-o <output>] [-i <image.png>] [-m <stretch|fill|fit|center|tile|solid_color>] [-c <#rrggbb>]...",
            program.to_string_lossy()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AppearanceRules, BackgroundColor, BackgroundSource};

    struct Fixture;

    impl Fixture {
        fn args(values: &[&str]) -> Vec<std::ffi::OsString> {
            values.iter().map(std::ffi::OsString::from).collect()
        }
    }

    #[test]
    fn later_named_rule_overrides_the_wildcard() {
        let rules = AppearanceRules::parse(Fixture::args(&[
            "wallpaper",
            "-i",
            "all.png",
            "-m",
            "fill",
            "-o",
            "HDMI-A-1",
            "-i",
            "main.png",
        ]))
        .unwrap();
        assert_eq!(
            rules.for_output("DP-1").source,
            BackgroundSource::Image("all.png".into())
        );
        assert_eq!(
            rules.for_output("HDMI-A-1").source,
            BackgroundSource::Image("main.png".into())
        );
    }

    #[test]
    fn color_only_rule_is_solid_color() {
        let rules = AppearanceRules::parse(Fixture::args(&["wallpaper", "-c", "#123456"])).unwrap();
        let appearance = rules.for_output("anything");
        assert_eq!(appearance.source, BackgroundSource::SolidColor);
        assert_eq!(appearance.color, BackgroundColor::parse("123456").unwrap());
    }
}
