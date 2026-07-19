//! Per-module foreground colors, matching waybar's `style.css` rules
//! (`#clock`, `#battery.warning`, `#network.disconnected`, etc.).
//!
//! Pulled out of `bins/statusbar/src/main.rs` into the library crate so
//! `module_color`'s state-detection string matching (icon glyph suffix/
//! prefix checks, percentage-threshold parsing) is unit-testable without
//! needing a live Wayland connection -- these are the kind of "innocuous
//! string-matching helper" bugs that bit this bar twice already (a
//! hand-transcribed `\u{...}` escape and a mismatched icon codepoint both
//! shipped silently since nothing exercised them off a live compositor).

use platform::draw::Color;

/// Kanagawa Dragon accents, lifted verbatim from waybar's own
/// `~/.config/waybar/style.css` `@define-color` block -- so our bar's
/// per-module colors match waybar's exactly, not just the shape of the
/// idea. `fg`/`bg` aren't here: those already come from `[bar]`/`[colors]`
/// (see `cfg.bar_foreground`/`bar_background`), since unlike waybar's
/// hardcoded stylesheet, ours mirrors the terminal's configured theme by
/// default.
pub mod palette {
    use platform::draw::Color;

    pub const FG_DARK: Color = Color::rgb(0xa6, 0xa6, 0x9c);
    pub const RED: Color = Color::rgb(0xc4, 0x74, 0x6e);
    pub const ORANGE: Color = Color::rgb(0xb6, 0x92, 0x7b);
    pub const YELLOW: Color = Color::rgb(0xc4, 0xb2, 0x8a);
    pub const GREEN: Color = Color::rgb(0x8a, 0x9a, 0x7b);
    pub const BLUE: Color = Color::rgb(0x8b, 0xa4, 0xb0);
    pub const PURPLE: Color = Color::rgb(0xa2, 0x92, 0xa3);
    pub const TEAL: Color = Color::rgb(0x8e, 0xa4, 0xa2);
}

/// Which module a rendered segment came from -- just enough to pick its
/// color in [`module_color`]; not otherwise used for layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)] // Cpu/Mem: disabled in the bin's right_segments() per user request, kept for easy re-enable
pub enum ModuleKind {
    Clock,
    Date,
    Wifi,
    Bluetooth,
    Battery,
    Temp,
    Cpu,
    Mem,
    VolOut,
    VolIn,
    Brightness,
}

/// Per-module foreground color -- state is read straight off the icon glyph
/// each module already picked (`modules.rs`'s icons are unique per state:
/// disconnected/muted/charging all use a different glyph), so this needs no
/// extra plumbing back to the raw sensor data. Codepoints below were pulled
/// with a throwaway probe binary reading `modules.rs`'s actual string
/// literals, not hand-copied, after a hand-transcribed `\u{...}` escape
/// caused a tofu regression once already (see the CPU icon fix).
#[must_use]
pub fn module_color(kind: ModuleKind, text: &str) -> Color {
    use palette::{BLUE, FG_DARK, GREEN, ORANGE, PURPLE, RED, TEAL, YELLOW};
    match kind {
        ModuleKind::Clock | ModuleKind::Date => PURPLE,
        ModuleKind::Wifi => {
            if text.starts_with('\u{f092d}') {
                RED // waybar's #network.disconnected
            } else {
                BLUE
            }
        }
        ModuleKind::Bluetooth => {
            if text.starts_with('\u{f00b2}') {
                FG_DARK // waybar's #bluetooth.off/.disabled
            } else {
                PURPLE
            }
        }
        ModuleKind::Battery => {
            if text.ends_with('\u{f0084}') || text.ends_with('\u{f06a5}') {
                GREEN // waybar's #battery.charging/.plugged
            } else if let Some(pct) = trailing_percent(text) {
                if pct < 15 {
                    RED // waybar's #battery.critical
                } else if pct < 30 {
                    YELLOW // waybar's #battery.warning
                } else {
                    GREEN
                }
            } else {
                GREEN
            }
        }
        ModuleKind::Temp => ORANGE,
        ModuleKind::Cpu => GREEN,
        ModuleKind::Mem => BLUE,
        ModuleKind::VolOut => {
            if text.ends_with('\u{f075f}') {
                FG_DARK // waybar's #pulseaudio.muted
            } else {
                TEAL
            }
        }
        ModuleKind::VolIn => {
            if text.ends_with('\u{f036d}') {
                FG_DARK
            } else {
                ORANGE // waybar's #pulseaudio.microphone
            }
        }
        ModuleKind::Brightness => YELLOW,
    }
}

/// The percentage number in a module string like "X 87% 󰁹" (a number
/// followed by `%`, anywhere -- battery's actual format always trails the
/// percent field with an icon glyph, e.g. `"{cap:>3}% {icon}"`, so this
/// can't assume the string *ends* in `%`) -- used for battery's
/// warning/critical color thresholds.
///
/// # Bug this fixes
///
/// An earlier version only handled a string that literally *ends* in
/// `"N%"` (`text.trim_end_matches('%').rsplit(' ').next()`) -- which
/// never matches `battery()`'s real output, since it always appends an
/// icon glyph after the `%` sign. That meant the critical/warning color
/// thresholds below silently never fired in production: every battery
/// level rendered green regardless of charge, and nothing caught it
/// because there was no unit test exercising the function against the
/// real string shape -- only a hand-picked "N%" fixture that happened to
/// dodge the bug. Found by writing exactly that test.
#[must_use]
pub fn trailing_percent(text: &str) -> Option<u32> {
    text.split_whitespace()
        .find_map(|field| field.strip_suffix('%')?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_percent_parses_field_before_a_trailing_icon() {
        // Regression test: battery()'s real format is "{cap:>3}% {icon}",
        // i.e. the percent field is NOT the last whitespace-field -- an
        // icon glyph always follows it. This must still find it.
        assert_eq!(trailing_percent("100% 󰁹"), Some(100));
        assert_eq!(trailing_percent(" 87%"), Some(87));
        assert_eq!(trailing_percent("0%"), Some(0));
        assert_eq!(trailing_percent("  5% 󰂄"), Some(5));
    }

    #[test]
    fn trailing_percent_rejects_non_numeric() {
        assert_eq!(trailing_percent("—"), None);
        assert_eq!(trailing_percent(""), None);
    }

    #[test]
    fn clock_and_date_are_purple() {
        assert_eq!(module_color(ModuleKind::Clock, "12:00:00"), palette::PURPLE);
        assert_eq!(module_color(ModuleKind::Date, "2024-01-01"), palette::PURPLE);
    }

    #[test]
    fn wifi_disconnected_is_red() {
        let disconnected = "\u{f092d}";
        assert_eq!(module_color(ModuleKind::Wifi, disconnected), palette::RED);
    }

    #[test]
    fn wifi_connected_is_blue() {
        let connected = "myssid \u{f1eb}";
        assert_eq!(module_color(ModuleKind::Wifi, connected), palette::BLUE);
    }

    #[test]
    fn bluetooth_off_is_dim() {
        let off = "\u{f00b2}";
        assert_eq!(module_color(ModuleKind::Bluetooth, off), palette::FG_DARK);
    }

    #[test]
    fn bluetooth_on_is_purple() {
        let on = "\u{f00af}";
        assert_eq!(module_color(ModuleKind::Bluetooth, on), palette::PURPLE);
    }

    #[test]
    fn battery_charging_is_green_regardless_of_percent() {
        let charging = "  5% \u{f0084}";
        assert_eq!(module_color(ModuleKind::Battery, charging), palette::GREEN);
    }

    #[test]
    fn battery_critical_is_red() {
        assert_eq!(module_color(ModuleKind::Battery, " 10% \u{f007a}"), palette::RED);
    }

    #[test]
    fn battery_warning_is_yellow() {
        assert_eq!(module_color(ModuleKind::Battery, " 20% \u{f007a}"), palette::YELLOW);
    }

    #[test]
    fn battery_healthy_is_green() {
        assert_eq!(module_color(ModuleKind::Battery, " 80% \u{f0079}"), palette::GREEN);
    }

    #[test]
    fn battery_threshold_boundaries_are_exclusive_upper() {
        // pct < 15 -> red; pct == 15 -> not red (yellow band).
        assert_eq!(module_color(ModuleKind::Battery, " 15% \u{f007a}"), palette::YELLOW);
        // pct < 30 -> yellow; pct == 30 -> green.
        assert_eq!(module_color(ModuleKind::Battery, " 30% \u{f007a}"), palette::GREEN);
    }

    #[test]
    fn vol_out_muted_is_dim() {
        let muted = "  0% \u{f075f}";
        assert_eq!(module_color(ModuleKind::VolOut, muted), palette::FG_DARK);
    }

    #[test]
    fn vol_out_unmuted_is_teal() {
        let unmuted = " 50% \u{f057e}";
        assert_eq!(module_color(ModuleKind::VolOut, unmuted), palette::TEAL);
    }

    #[test]
    fn vol_in_muted_is_dim() {
        let muted = "  0% \u{f036d}";
        assert_eq!(module_color(ModuleKind::VolIn, muted), palette::FG_DARK);
    }

    #[test]
    fn vol_in_unmuted_is_orange() {
        let unmuted = " 50% \u{f036c}";
        assert_eq!(module_color(ModuleKind::VolIn, unmuted), palette::ORANGE);
    }

    #[test]
    fn temp_cpu_mem_brightness_are_fixed_colors() {
        assert_eq!(module_color(ModuleKind::Temp, " 50°C"), palette::ORANGE);
        assert_eq!(module_color(ModuleKind::Cpu, " 50%"), palette::GREEN);
        assert_eq!(module_color(ModuleKind::Mem, "1.0/2.0GiB"), palette::BLUE);
        assert_eq!(module_color(ModuleKind::Brightness, " 50%"), palette::YELLOW);
    }
}
