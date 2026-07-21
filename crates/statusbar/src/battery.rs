//! Laptop battery percentage + charge state, from `/sys/class/power_supply`.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh sysfs read, there's no state to hold between polls.

use crate::sys_file::ReadTrimmed;
use std::fs;

/// Battery percentage + charge-state icon -- see the module doc for why
/// this is a marker type.
pub struct Battery;

impl Battery {
    /// First battery under `/sys/class/power_supply/BAT*`: percentage +
    /// charge state ("Charging"/"Discharging"/"Full"/...). `None` on a
    /// desktop with no battery.
    #[must_use]
    pub fn read() -> Option<String> {
        let dir = fs::read_dir("/sys/class/power_supply").ok()?;
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("BAT") {
                continue;
            }
            let path = entry.path();
            let cap = path.join("capacity").read_trim()?;
            let status = path
                .join("status")
                .read_trim()
                .unwrap_or_else(|| "Unknown".into());
            let cap_n: u32 = cap.parse().unwrap_or(0);
            let icon = match status.as_str() {
                "Charging" => "󰂄",
                "Full" | "Not charging" if cap_n >= 95 => "󰚥",
                _ => Self::ramp_icon(cap_n),
            };
            return Some(format!("{cap:>3}% {icon}"));
        }
        None
    }

    /// Battery-level ramp icon for a plain (not charging/full) capacity
    /// percentage -- an 11-step Nerd Font ramp, matching waybar's own
    /// `battery.format-icons` list.
    fn ramp_icon(cap: u32) -> &'static str {
        const RAMP: [&str; 11] = ["󰂎", "󰁺", "󰁻", "󰁼", "󰁽", "󰁾", "󰁿", "󰂀", "󰂁", "󰂂", "󰁹"];
        let idx = ((cap as usize) * (RAMP.len() - 1)) / 100;
        RAMP[idx.min(RAMP.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_ramp_bottom_and_top_are_distinct_and_stable() {
        // Regression guard rather than a hand-copied codepoint fixture (an
        // earlier attempt at this exact test hand-transcribed the ramp
        // array and got it wrong -- see the CPU/wifi icon bugs this crate
        // has already shipped from the same mistake). 0% and 100% must map
        // to *some* two distinct, stable icons.
        let bottom = Battery::ramp_icon(0);
        let top = Battery::ramp_icon(100);
        assert_ne!(bottom, top);
        assert_eq!(Battery::ramp_icon(0), bottom, "not deterministic");
        assert_eq!(Battery::ramp_icon(100), top, "not deterministic");
    }

    #[test]
    fn battery_ramp_is_monotonic_by_index() {
        // The ramp must never regress to an *earlier* icon as capacity
        // rises -- walk the 0..=100 domain and check each new icon we see
        // is either the same as the last or the next one, never a jump
        // backward to one we'd already left behind.
        let mut seen_order: Vec<&str> = Vec::new();
        for cap in 0..=100u32 {
            let icon = Battery::ramp_icon(cap);
            if seen_order.last() != Some(&icon) {
                assert!(
                    !seen_order.contains(&icon),
                    "ramp regressed to an earlier icon at cap={cap}"
                );
                seen_order.push(icon);
            }
        }
        // 11-step ramp: expect to actually traverse a nontrivial number of
        // distinct icons across the full range, not get stuck on one.
        assert!(seen_order.len() >= 8, "ramp barely moves: {seen_order:?}");
    }

    #[test]
    fn battery_ramp_never_panics_past_100() {
        // Defensive: cap should never exceed 100 in practice (it's a sysfs
        // percentage), but the `.min(RAMP.len() - 1)` clamp should hold even
        // if it somehow does.
        let _ = Battery::ramp_icon(255);
    }
}
