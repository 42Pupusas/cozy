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
use std::process::{Command, Stdio};
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
    Some(format!("{ssid} 󰤨"))
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
        Some(format!("{n} 󰂱"))
    }
}

/// Default sink (output) volume percentage + mute state, via `pactl` (falls
/// back to `amixer` if `pactl` isn't found).
#[must_use]
pub fn volume_out() -> Option<String> {
    volume_out_raw().map(|(s, ..)| s)
}

/// Same as [`volume_out`] but also hands back the bare `(pct, muted)` pair --
/// so a caller applying an optimistic scroll-delta (see the statusbar
/// worker) can compute the next percentage without re-parsing the rendered
/// string.
#[must_use]
pub fn volume_out_raw() -> Option<(String, u32, bool)> {
    if let Some(v) = pactl_volume_raw("@DEFAULT_SINK@") {
        return Some(v);
    }
    amixer_volume_raw("Master")
}

/// Default source (input/mic) volume percentage + mute state, via `pactl`.
#[must_use]
pub fn volume_in() -> Option<String> {
    volume_in_raw().map(|(s, ..)| s)
}

/// See [`volume_out_raw`].
#[must_use]
pub fn volume_in_raw() -> Option<(String, u32, bool)> {
    if let Some(v) = pactl_volume_raw("@DEFAULT_SOURCE@") {
        return Some(v);
    }
    amixer_volume_raw("Capture")
}

/// Nudge the default sink (output) volume by `delta_pct` percentage points
/// (negative to lower), via `pactl` (falls back to `amixer`). Clamped to
/// `0..=100` by the underlying tool, not here. Fire-and-forget: errors are
/// swallowed since the next module poll will just show whatever the mixer
/// actually landed on.
pub fn adjust_volume_out(delta_pct: i32) {
    adjust_volume("@DEFAULT_SINK@", "Master", delta_pct);
}

/// Nudge the default source (input/mic) volume -- see [`adjust_volume_out`].
pub fn adjust_volume_in(delta_pct: i32) {
    adjust_volume("@DEFAULT_SOURCE@", "Capture", delta_pct);
}

fn adjust_volume(pactl_target: &str, amixer_control: &str, delta_pct: i32) {
    let kind = if pactl_target.contains("SOURCE") {
        "set-source-volume"
    } else {
        "set-sink-volume"
    };
    let sign = if delta_pct >= 0 { "+" } else { "-" };
    let arg = format!("{sign}{}%", delta_pct.abs());
    // `spawn` + drop the child, not `status`/`output`: this runs on the bar's
    // single render thread, and a rapid wheel scroll fires this once per
    // detent -- blocking on a `pactl`/`amixer` round-trip (process spawn +
    // exec + a D-Bus/PulseAudio IPC hop, easily tens of ms) each time is
    // exactly what made scrolling feel janky: the whole paint loop stalled
    // on process I/O every tick instead of just kicking it off.  We don't
    // need the exit status here -- the very next module poll re-reads
    // whatever the mixer actually landed on regardless of whether this
    // particular command succeeded.
    let spawned = Command::new("pactl")
        .args([kind, pactl_target, &arg])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if spawned.is_err() {
        let _ = Command::new("amixer")
            .args(["set", amixer_control, &format!("{}%{sign}", delta_pct.abs())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn pactl_volume_raw(target: &str) -> Option<(String, u32, bool)> {
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
    let is_source = target.contains("SOURCE");
    Some((format_volume(is_source, pct, muted), pct, muted))
}

fn amixer_volume_raw(control: &str) -> Option<(String, u32, bool)> {
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
    let is_source = control == "Capture";
    Some((format_volume(is_source, pct, muted), pct, muted))
}

/// Render a volume/mic percentage + mute state the same way
/// [`volume_out`]/[`volume_in`] do -- exposed `pub` (not just for this
/// module's own callers) so a caller that already knows the percentage
/// in hand (e.g. an optimistic UI update applied before the real `pactl`
/// mutation has even been spawned, let alone read back) can format it
/// identically without shelling out.
#[must_use]
pub fn format_volume(is_source: bool, pct: u32, muted: bool) -> String {
    let icon = pick_volume_icon(is_source, pct, muted);
    format!("{pct:>3}% {icon}")
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
const BRIGHTNESS_ICON: &str = "󰃠";

/// Screen backlight brightness percentage, from the first
/// `/sys/class/backlight/*` device.
#[must_use]
pub fn brightness() -> Option<String> {
    brightness_raw().map(|(s, ..)| s)
}

/// Same as [`brightness`] but also hands back the bare percentage -- see
/// [`volume_out_raw`] for why.
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
/// `format_volume` for why this is `pub`.
#[must_use]
pub fn format_brightness(pct: u32) -> String {
    format!("{pct:>3}% {BRIGHTNESS_ICON}")
}

/// Nudge screen backlight brightness by `delta_pct` percentage points
/// (negative to dim) via a direct `/sys/class/backlight/*/brightness`
/// read-modify-write (works without root when a udev rule grants the
/// `video` group write access, which is the common desktop default -- e.g.
/// `73-seat-backlight.rules` upstream udev/systemd). Not `brightnessctl`:
/// its relative-delta syntax is asymmetric (`+N%` to raise but `N%-`, not
/// `-N%`, to lower) and got this wrong in an earlier version, silently
/// no-opping every dim adjustment.
pub fn adjust_brightness(delta_pct: i32) {
    // `brightnessctl`'s relative-delta syntax puts the sign in different
    // places depending on direction: `+10%` to raise, but `10%-` (sign
    // *trailing*, magnitude first) to lower -- `-10%` is not valid and
    // brightnessctl rejects it as a bad argument, which combined with
    // stderr being nulled made every dim-scroll a silent no-op. Rather than
    // rely on getting that asymmetric syntax right (and on brightnessctl
    // being installed at all), just do the sysfs read-modify-write
    // directly -- it's a couple of small file reads/writes, cheap enough
    // to not need `spawn`'s fire-and-forget treatment, and it's already
    // `brightness()`'s own read path so behavior stays consistent between
    // the two.
    let Ok(dir) = fs::read_dir("/sys/class/backlight") else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
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
        let step = if delta_pct >= 0 { magnitude } else { -magnitude };
        let new_val = (cur + step).clamp(0, max);
        let _ = fs::write(path.join("brightness"), new_val.to_string());
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

    // pick_volume_icon / format_volume

    #[test]
    fn pick_volume_icon_sink_thresholds() {
        assert_eq!(pick_volume_icon(false, 0, false), "\u{f057f}");
        assert_eq!(pick_volume_icon(false, 32, false), "\u{f057f}");
        assert_eq!(pick_volume_icon(false, 33, false), "\u{f0580}");
        assert_eq!(pick_volume_icon(false, 65, false), "\u{f0580}");
        assert_eq!(pick_volume_icon(false, 66, false), "\u{f057e}");
        assert_eq!(pick_volume_icon(false, 100, false), "\u{f057e}");
    }

    #[test]
    fn pick_volume_icon_muted_overrides_level() {
        assert_eq!(pick_volume_icon(false, 100, true), "\u{f075f}");
    }

    #[test]
    fn pick_volume_icon_source_ignores_level_thresholds() {
        assert_eq!(pick_volume_icon(true, 5, false), "\u{f036c}");
        assert_eq!(pick_volume_icon(true, 100, false), "\u{f036c}");
        assert_eq!(pick_volume_icon(true, 50, true), "\u{f036d}");
    }

    #[test]
    fn format_volume_pads_to_three_digits() {
        assert_eq!(format_volume(false, 7, false), "  7% \u{f057f}");
        assert_eq!(format_volume(false, 100, false), "100% \u{f057e}");
    }
}
