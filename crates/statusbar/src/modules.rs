//! Status-bar modules.
//!
//! Each module is a stateless-ish `fn() -> Option<String>` reading straight
//! from `/proc`/`/sys`/libc (or shelling a system tool where neither exposes
//! the number directly, e.g. mixer volume). None of these need root or any
//! new crate dependency — everything here is a `/proc`/`/sys` text file, a
//! raw libc call (see `crate::localtime`), or a well-known CLI already on a
//! desktop Linux box.
//!
//! Fallible by design: a laptop-only module (battery) on a desktop, or a
//! missing CLI (bluetoothctl not installed), just returns `None` and the bar
//! renders that slot as an em-dash instead of erroring the whole process.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock `HH:MM:SS`, local time — via `crate::localtime` (raw
/// `time(2)`/`localtime_r(3)`), not a `date` shell-out.
#[must_use]
pub fn clock() -> Option<String> {
    crate::localtime::clock()
}

/// Date, `YYYY-MM-DD`.
#[must_use]
pub fn date() -> Option<String> {
    crate::localtime::date()
}

/// First battery under `/sys/class/power_supply/BAT*`: percentage + charge
/// state ("Charging"/"Discharging"/"Full"/...). `None` on a desktop with no
/// battery.
#[must_use]
pub fn battery() -> Option<String> {
    let dir = fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("BAT") {
            continue;
        }
        let path = entry.path();
        let cap = read_trim(path.join("capacity"))?;
        let status = read_trim(path.join("status")).unwrap_or_else(|| "Unknown".into());
        let cap_n: u32 = cap.parse().unwrap_or(0);
        let icon = match status.as_str() {
            "Charging" => "󰂄",
            "Full" | "Not charging" if cap_n >= 95 => "󰚥",
            _ => battery_ramp_icon(cap_n),
        };
        return Some(format!("{cap:>3}% {icon}"));
    }
    None
}

/// Battery-level ramp icon for a plain (not charging/full) capacity
/// percentage -- an 11-step Nerd Font ramp, matching waybar's own
/// `battery.format-icons` list.
fn battery_ramp_icon(cap: u32) -> &'static str {
    const RAMP: [&str; 11] = ["󰂎", "󰁺", "󰁻", "󰁼", "󰁽", "󰁾", "󰁿", "󰂀", "󰂁", "󰂂", "󰁹"];
    let idx = ((cap as usize) * (RAMP.len() - 1)) / 100;
    RAMP[idx.min(RAMP.len() - 1)]
}

/// CPU package/core temperature in °C, from the first
/// `/sys/class/thermal/thermal_zone*/temp` that parses (millidegrees).
#[must_use]
pub fn temperature() -> Option<String> {
    let dir = fs::read_dir("/sys/class/thermal").ok()?;
    for entry in dir.flatten() {
        let path = entry.path().join("temp");
        if let Some(raw) = read_trim(path) {
            if let Ok(milli) = raw.parse::<i64>() {
                return Some(format!("{:>3.0}°C", milli as f64 / 1000.0));
            }
        }
    }
    None
}

/// System-wide CPU utilization percentage over a short sampling window, from
/// two `/proc/stat` reads ~100ms apart. Blocks the calling thread for that
/// window — callers should poll this off the UI thread's hot path (e.g. a
/// dedicated module-refresh timer), not every frame.
#[must_use]
pub fn cpu_usage() -> Option<String> {
    let a = read_proc_stat_cpu()?;
    std::thread::sleep(std::time::Duration::from_millis(120));
    let b = read_proc_stat_cpu()?;
    let idle_delta = b.idle.saturating_sub(a.idle);
    let total_delta = b.total.saturating_sub(a.total);
    if total_delta == 0 {
        return Some("0%".into());
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = 100.0 * (1.0 - idle_delta as f64 / total_delta as f64);
    Some(format!("{pct:>3.0}% "))
}

struct CpuTimes {
    idle: u64,
    total: u64,
}

fn read_proc_stat_cpu() -> Option<CpuTimes> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let line = text.lines().next()?; // "cpu  user nice system idle iowait irq softirq steal ..."
    let mut fields = line.split_whitespace();
    fields.next()?; // "cpu"
    let nums: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
    if nums.len() < 4 {
        return None;
    }
    let idle = nums[3] + nums.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = nums.iter().sum();
    Some(CpuTimes { idle, total })
}

/// Used/total RAM in GiB, from `/proc/meminfo`.
#[must_use]
pub fn memory() -> Option<String> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = None;
    let mut avail_kb = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = parse_kb(rest);
        }
    }
    let (total, avail) = (total_kb?, avail_kb?);
    let used_gib = (total - avail) as f64 / 1024.0 / 1024.0;
    let total_gib = total as f64 / 1024.0 / 1024.0;
    Some(format!("{used_gib:>4.1}/{total_gib:>4.1}GiB 󰅛"))
}

fn parse_kb(s: &str) -> Option<u64> {
    s.trim()
        .strip_suffix(" kB")
        .unwrap_or(s.trim())
        .trim()
        .parse()
        .ok()
}

/// Connected Wi-Fi SSID + a signal-strength percentage, via NetworkManager
/// over D-Bus (`dbus::network_manager::wifi_status`) instead of shelling out
/// to `iw` and scraping its text output. `None` only when the system bus /
/// NetworkManager aren't reachable at all; a Wi-Fi device present but not
/// associated to an access point still renders the disconnected icon
/// (matches the old `iw`-based "Not connected" case, not a `None`).
#[must_use]
pub fn wifi() -> Option<String> {
    let mut conn = dbus::DbusConn::system().ok()?;
    match dbus::network_manager::wifi_status(&mut conn).ok()? {
        Some(status) => Some(format!("{} 󰤨", status.ssid)),
        None => Some("󰤭".into()),
    }
}

/// Whether Bluetooth is powered and how many devices are connected, via
/// bluez's `ObjectManager.GetManagedObjects` over D-Bus (`dbus::bluez::status`)
/// instead of shelling out to `bluetoothctl` and scraping its text output.
/// `None` if the system bus / bluez aren't reachable, or no adapter exists.
#[must_use]
pub fn bluetooth() -> Option<String> {
    let mut conn = dbus::DbusConn::system().ok()?;
    let status = dbus::bluez::status(&mut conn).ok()??;
    if !status.powered {
        return Some("󰂲".into());
    }
    if status.connected_count == 0 {
        Some("󰂯".into())
    } else {
        Some(format!("{} 󰂱", status.connected_count))
    }
}

const BRIGHTNESS_ICON: &str = "󰃠";

/// Screen backlight brightness percentage, from the first
/// `/sys/class/backlight/*` device.
#[must_use]
pub fn brightness() -> Option<String> {
    brightness_raw().map(|(s, ..)| s)
}

/// Same as [`brightness`] but also hands back the bare percentage -- see
/// [`crate::volume::Volume::sink_raw`] for why.
#[must_use]
pub fn brightness_raw() -> Option<(String, u32)> {
    let dir = fs::read_dir("/sys/class/backlight").ok()?;
    for entry in dir.flatten() {
        let path = entry.path();
        let cur: f64 = read_trim(path.join("brightness"))?.parse().ok()?;
        let max: f64 = read_trim(path.join("max_brightness"))?.parse().ok()?;
        if max <= 0.0 {
            continue;
        }
        let pct = (cur / max * 100.0).round() as u32;
        return Some((format_brightness(pct), pct));
    }
    None
}

/// Render a brightness percentage the same way `brightness()` does -- see
/// `crate::volume::Volume::format` for why this is `pub`.
#[must_use]
pub fn format_brightness(pct: u32) -> String {
    format!("{pct:>3}% {BRIGHTNESS_ICON}")
}

/// Nudge screen backlight brightness by `delta_pct` percentage points
/// (negative to dim), via `logind`'s `Session.SetBrightness` D-Bus call
/// (`dbus::logind::set_backlight_brightness`) rather than a direct
/// `/sys/class/backlight/*/brightness` write.
///
/// A direct sysfs write only works unprivileged when a udev rule has
/// granted the `video` group (or an ACL/uaccess tag) write access to that
/// file -- on a stock install without that rule (as on this machine: the
/// `video` group has no regular users in it, only `greeter`), the write
/// silently fails with a permission error that a bare `fs::write` swallows
/// via `let _ =`. `logind` is the actual sanctioned unprivileged path: it's
/// polkit-gated instead of file-permission-gated, and the default polkit
/// policy allows an active local session to call it without a password
/// prompt. Not `brightnessctl` either: its relative-delta syntax is
/// asymmetric (`+N%` to raise but `N%-`, not `-N%`, to lower) and got this
/// wrong in an earlier version, silently no-opping every dim adjustment.
pub fn adjust_brightness(delta_pct: i32) {
    let Ok(dir) = fs::read_dir("/sys/class/backlight") else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(cur) = read_trim(path.join("brightness")).and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        let Some(max) = read_trim(path.join("max_brightness")).and_then(|s| s.parse::<i64>().ok())
        else {
            continue;
        };
        if max <= 0 {
            continue;
        }
        let magnitude = (max * i64::from(delta_pct.abs()) / 100).max(1);
        let step = if delta_pct >= 0 {
            magnitude
        } else {
            -magnitude
        };
        let new_val = (cur + step).clamp(0, max);
        if let Ok(mut conn) = dbus::DbusConn::system() {
            #[allow(clippy::cast_sign_loss)]
            let _ = dbus::logind::set_backlight_brightness(&mut conn, name, new_val as u32);
        }
        return;
    }
}

fn read_trim(path: impl AsRef<std::path::Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Unix timestamp in whole seconds — a cheap monotonic-ish tick used by the
/// bar's refresh scheduler to decide "has at least N seconds passed".
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // battery_ramp_icon

    #[test]
    fn battery_ramp_bottom_and_top_are_distinct_and_stable() {
        // Regression guard rather than a hand-copied codepoint fixture (an
        // earlier attempt at this exact test hand-transcribed the ramp
        // array and got it wrong -- see the CPU/wifi icon bugs this crate
        // has already shipped from the same mistake). 0% and 100% must map
        // to *some* two distinct, stable icons.
        let bottom = battery_ramp_icon(0);
        let top = battery_ramp_icon(100);
        assert_ne!(bottom, top);
        assert_eq!(battery_ramp_icon(0), bottom, "not deterministic");
        assert_eq!(battery_ramp_icon(100), top, "not deterministic");
    }

    #[test]
    fn battery_ramp_is_monotonic_by_index() {
        // The ramp must never regress to an *earlier* icon as capacity
        // rises -- walk the 0..=100 domain and check each new icon we see
        // is either the same as the last or the next one, never a jump
        // backward to one we'd already left behind.
        let mut seen_order: Vec<&str> = Vec::new();
        for cap in 0..=100u32 {
            let icon = battery_ramp_icon(cap);
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
        let _ = battery_ramp_icon(255);
    }

    // parse_kb

    #[test]
    fn parse_kb_strips_suffix_and_whitespace() {
        assert_eq!(parse_kb("   16384000 kB"), Some(16_384_000));
        assert_eq!(parse_kb("1024 kB"), Some(1024));
    }

    #[test]
    fn parse_kb_without_suffix_still_parses() {
        assert_eq!(parse_kb("  2048  "), Some(2048));
    }

    #[test]
    fn parse_kb_garbage_is_none() {
        assert_eq!(parse_kb("not a number"), None);
        assert_eq!(parse_kb(""), None);
    }

}
