//! Status-bar modules.
//!
//! Each module is a stateless-ish `fn() -> Option<String>` reading straight
//! from `/proc`/`/sys` (or shelling a system tool where the kernel doesn't
//! expose the number directly, e.g. mixer volume). None of these need root or
//! any new crate dependency — everything here is either a `/proc`/`/sys`
//! text file or a well-known CLI already on a desktop Linux box.
//!
//! Fallible by design: a laptop-only module (battery) on a desktop, or a
//! missing CLI (bluetoothctl not installed), just returns `None` and the bar
//! renders that slot as an em-dash instead of erroring the whole process.

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wall-clock `HH:MM:SS`, local time — via `date`, not a chrono dependency
/// (the whole point of reusing this crate's zero-extra-deps posture).
#[must_use]
pub fn clock() -> Option<String> {
    let out = Command::new("date").arg("+%H:%M:%S").output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_string())
}

/// Date, `YYYY-MM-DD`.
#[must_use]
pub fn date() -> Option<String> {
    let out = Command::new("date").arg("+%Y-%m-%d").output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_string())
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
        return Some(format!("{icon} {cap}%"));
    }
    None
}

/// Battery-level ramp icon for a plain (not charging/full) capacity
/// percentage -- an 11-step Nerd Font ramp, matching waybar's own
/// `battery.format-icons` list.
fn battery_ramp_icon(cap: u32) -> &'static str {
    const RAMP: [&str; 11] = [
        "󰂎", "󰁺", "󰁻", "󰁼", "󰁽", "󰁾", "󰁿", "󰂀", "󰂁", "󰂂", "󰁹",
    ];
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
                return Some(format!("󰃈 {:.0}°C", milli as f64 / 1000.0));
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
    Some(format!("󴢼 {pct:.0}%"))
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
    Some(format!("󰅛 {used_gib:.1}/{total_gib:.1}GiB"))
}

fn parse_kb(s: &str) -> Option<u64> {
    s.trim().strip_suffix(" kB").unwrap_or(s.trim()).trim().parse().ok()
}

/// Connected Wi-Fi SSID + a signal-strength percentage, via `iw`. `None` when
/// no wireless interface is connected or `iw` isn't installed.
#[must_use]
pub fn wifi() -> Option<String> {
    // Find the first wireless interface.
    let dev_out = Command::new("iw").arg("dev").output().ok()?;
    let dev_text = String::from_utf8(dev_out.stdout).ok()?;
    let iface = dev_text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Interface "))?
        .trim()
        .to_string();

    let link_out = Command::new("iw")
        .args(["dev", &iface, "link"])
        .output()
        .ok()?;
    let link_text = String::from_utf8(link_out.stdout).ok()?;
    if link_text.contains("Not connected") {
        return Some("󰤭".into());
    }
    let ssid = link_text
        .lines()
        .find_map(|l| l.trim().strip_prefix("SSID: "))
        .unwrap_or("?")
        .to_string();
    Some(format!("󰤨 {ssid}"))
}

/// Whether Bluetooth is powered and how many devices are connected, via
/// `bluetoothctl`. `None` if the daemon/CLI isn't available.
#[must_use]
pub fn bluetooth() -> Option<String> {
    let out = Command::new("bluetoothctl").arg("show").output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let powered = text.lines().any(|l| l.trim() == "Powered: yes");
    if !powered {
        return Some("󰂲".into());
    }
    let devices_out = Command::new("bluetoothctl")
        .args(["devices", "Connected"])
        .output()
        .ok()?;
    let n = String::from_utf8(devices_out.stdout)
        .ok()?
        .lines()
        .filter(|l| l.starts_with("Device"))
        .count();
    if n == 0 {
        Some("󰂯".into())
    } else {
        Some(format!("󰂱 {n}"))
    }
}

/// Default sink (output) volume percentage + mute state, via `pactl` (falls
/// back to `amixer` if `pactl` isn't found).
#[must_use]
pub fn volume_out() -> Option<String> {
    if let Some(v) = pactl_volume("@DEFAULT_SINK@") {
        return Some(v);
    }
    amixer_volume("Master")
}

/// Default source (input/mic) volume percentage + mute state, via `pactl`.
#[must_use]
pub fn volume_in() -> Option<String> {
    if let Some(v) = pactl_volume("@DEFAULT_SOURCE@") {
        return Some(v);
    }
    amixer_volume("Capture")
}

fn pactl_volume(target: &str) -> Option<String> {
    let kind = if target.contains("SOURCE") {
        "get-source-volume"
    } else {
        "get-sink-volume"
    };
    let vol_out = Command::new("pactl").args([kind, target]).output().ok()?;
    let vol_text = String::from_utf8(vol_out.stdout).ok()?;
    let pct = vol_text
        .split('/')
        .find_map(|part| part.trim().strip_suffix('%'))
        .and_then(|n| n.trim().parse::<u32>().ok())?;

    let mute_kind = if target.contains("SOURCE") {
        "get-source-mute"
    } else {
        "get-sink-mute"
    };
    let mute_out = Command::new("pactl")
        .args([mute_kind, target])
        .output()
        .ok()?;
    let muted = String::from_utf8(mute_out.stdout)
        .unwrap_or_default()
        .contains("yes");
    Some(format_volume(target.contains("SOURCE"), pct, muted))
}

fn amixer_volume(control: &str) -> Option<String> {
    let out = Command::new("amixer")
        .args(["get", control])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let line = text.lines().rfind(|l| l.contains('%'))?;
    let pct = line
        .split('[')
        .nth(1)?
        .split('%')
        .next()?
        .parse::<u32>()
        .ok()?;
    let muted = line.contains("[off]");
    Some(format_volume(control == "Capture", pct, muted))
}

fn format_volume(is_source: bool, pct: u32, muted: bool) -> String {
    let icon = pick_volume_icon(is_source, pct, muted);
    format!("{icon} {pct}%")
}

fn pick_volume_icon(is_source: bool, pct: u32, muted: bool) -> &'static str {
    if is_source {
        return if muted { MIC_MUTED } else { MIC_ON };
    }
    if muted {
        return VOL_MUTED;
    }
    if pct < 33 {
        VOL_LOW
    } else if pct < 66 {
        VOL_MED
    } else {
        VOL_HIGH
    }
}

const MIC_ON: &str = "󰍬";
const MIC_MUTED: &str = "󰍭";
const VOL_LOW: &str = "󰕿";
const VOL_MED: &str = "󰖀";
const VOL_HIGH: &str = "󰕾";
const VOL_MUTED: &str = "󰝟";

/// Screen backlight brightness percentage, from the first
/// `/sys/class/backlight/*` device.
#[must_use]
pub fn brightness() -> Option<String> {
    let dir = fs::read_dir("/sys/class/backlight").ok()?;
    for entry in dir.flatten() {
        let path = entry.path();
        let cur: f64 = read_trim(path.join("brightness"))?.parse().ok()?;
        let max: f64 = read_trim(path.join("max_brightness"))?.parse().ok()?;
        if max <= 0.0 {
            continue;
        }
        let pct = (cur / max * 100.0).round() as u32;
        return Some(format!("󰃠 {pct}%"));
    }
    None
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
