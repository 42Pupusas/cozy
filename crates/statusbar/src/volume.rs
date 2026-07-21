//! Mixer volume (sink/output and source/input) — read + scroll-adjust, via
//! `pactl` (falls back to `amixer` if `pactl` isn't found).
//!
//! Split out of `modules.rs` as its own owner, [`Volume`], since it was by
//! far the biggest cluster of stateless helpers in that file (query, raw
//! query, adjust, format, icon-pick, `pactl`/`amixer` backends -- ten
//! functions all operating on the same two-target/two-backend shape). A
//! zero-sized marker type, not a value with fields: every call is a fresh
//! `pactl`/`amixer` round trip, there's no connection or state to hold
//! between calls (unlike `dbus::DbusConn`), so `Volume::sink()` etc. read
//! exactly like the free functions they replaced but now have one obvious
//! home instead of floating at module scope.

use std::process::{Command, Stdio};

const MIC_ON: &str = "󰍬";
const MIC_MUTED: &str = "󰍭";
const VOL_LOW: &str = "󰕿";
const VOL_MED: &str = "󰖀";
const VOL_HIGH: &str = "󰕾";
const VOL_MUTED: &str = "󰝟";

/// Mixer volume queries/adjustments — see the module doc for why this is a
/// marker type rather than a value.
pub struct Volume;

impl Volume {
    /// Default sink (output) volume percentage + mute state.
    #[must_use]
    pub fn sink() -> Option<String> {
        Self::sink_raw().map(|(s, ..)| s)
    }

    /// Same as [`Self::sink`] but also hands back the bare `(pct, muted)`
    /// pair -- so a caller applying an optimistic scroll-delta (see the
    /// statusbar worker) can compute the next percentage without
    /// re-parsing the rendered string.
    #[must_use]
    pub fn sink_raw() -> Option<(String, u32, bool)> {
        if let Some(v) = Self::pactl_raw("@DEFAULT_SINK@") {
            return Some(v);
        }
        Self::amixer_raw("Master")
    }

    /// Default source (input/mic) volume percentage + mute state.
    #[must_use]
    pub fn source() -> Option<String> {
        Self::source_raw().map(|(s, ..)| s)
    }

    /// See [`Self::sink_raw`].
    #[must_use]
    pub fn source_raw() -> Option<(String, u32, bool)> {
        if let Some(v) = Self::pactl_raw("@DEFAULT_SOURCE@") {
            return Some(v);
        }
        Self::amixer_raw("Capture")
    }

    /// Nudge the default sink (output) volume by `delta_pct` percentage
    /// points (negative to lower). Clamped to `0..=100` by the underlying
    /// tool, not here. Fire-and-forget: errors are swallowed since the next
    /// module poll will just show whatever the mixer actually landed on.
    pub fn adjust_sink(delta_pct: i32) {
        Self::adjust("@DEFAULT_SINK@", "Master", delta_pct);
    }

    /// Nudge the default source (input/mic) volume -- see
    /// [`Self::adjust_sink`].
    pub fn adjust_source(delta_pct: i32) {
        Self::adjust("@DEFAULT_SOURCE@", "Capture", delta_pct);
    }

    fn adjust(pactl_target: &str, amixer_control: &str, delta_pct: i32) {
        let kind = if pactl_target.contains("SOURCE") {
            "set-source-volume"
        } else {
            "set-sink-volume"
        };
        let sign = if delta_pct >= 0 { "+" } else { "-" };
        let arg = format!("{sign}{}%", delta_pct.abs());
        // `spawn` + drop the child, not `status`/`output`: this runs on the
        // bar's single render thread, and a rapid wheel scroll fires this
        // once per detent -- blocking on a `pactl`/`amixer` round-trip
        // (process spawn + exec + a D-Bus/PulseAudio IPC hop, easily tens
        // of ms) each time is exactly what made scrolling feel janky: the
        // whole paint loop stalled on process I/O every tick instead of
        // just kicking it off. We don't need the exit status here -- the
        // very next module poll re-reads whatever the mixer actually
        // landed on regardless of whether this particular command
        // succeeded.
        let spawned = Command::new("pactl")
            .args([kind, pactl_target, &arg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_err() {
            let _ = Command::new("amixer")
                .args([
                    "set",
                    amixer_control,
                    &format!("{}%{sign}", delta_pct.abs()),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }

    fn pactl_raw(target: &str) -> Option<(String, u32, bool)> {
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
        Some((Self::format(is_source, pct, muted), pct, muted))
    }

    fn amixer_raw(control: &str) -> Option<(String, u32, bool)> {
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
        Some((Self::format(is_source, pct, muted), pct, muted))
    }

    /// Render a volume/mic percentage + mute state the same way
    /// [`Self::sink`]/[`Self::source`] do -- exposed `pub` (not just for
    /// this module's own callers) so a caller that already knows the
    /// percentage in hand (e.g. an optimistic UI update applied before the
    /// real `pactl` mutation has even been spawned, let alone read back)
    /// can format it identically without shelling out.
    #[must_use]
    pub fn format(is_source: bool, pct: u32, muted: bool) -> String {
        let icon = Self::pick_icon(is_source, pct, muted);
        format!("{pct:>3}% {icon}")
    }

    fn pick_icon(is_source: bool, pct: u32, muted: bool) -> &'static str {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_volume_icon_sink_thresholds() {
        assert_eq!(Volume::pick_icon(false, 0, false), "\u{f057f}");
        assert_eq!(Volume::pick_icon(false, 32, false), "\u{f057f}");
        assert_eq!(Volume::pick_icon(false, 33, false), "\u{f0580}");
        assert_eq!(Volume::pick_icon(false, 65, false), "\u{f0580}");
        assert_eq!(Volume::pick_icon(false, 66, false), "\u{f057e}");
        assert_eq!(Volume::pick_icon(false, 100, false), "\u{f057e}");
    }

    #[test]
    fn pick_volume_icon_muted_overrides_level() {
        assert_eq!(Volume::pick_icon(false, 100, true), "\u{f075f}");
    }

    #[test]
    fn pick_volume_icon_source_ignores_level_thresholds() {
        assert_eq!(Volume::pick_icon(true, 5, false), "\u{f036c}");
        assert_eq!(Volume::pick_icon(true, 100, false), "\u{f036c}");
        assert_eq!(Volume::pick_icon(true, 50, true), "\u{f036d}");
    }

    #[test]
    fn format_volume_pads_to_three_digits() {
        assert_eq!(Volume::format(false, 7, false), "  7% \u{f057f}");
        assert_eq!(Volume::format(false, 100, false), "100% \u{f057e}");
    }
}
