//! Screen backlight brightness -- read from `/sys/class/backlight`, adjusted
//! via `logind`'s `Session.SetBrightness` D-Bus call.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh sysfs read or D-Bus round trip.

use crate::sys_file::ReadTrimmed;
use std::fs;

const ICON: &str = "󰃠";

/// Brightness queries/adjustments -- see the module doc for why this is a
/// marker type.
pub struct Brightness;

impl Brightness {
    /// Screen backlight brightness percentage, from the first
    /// `/sys/class/backlight/*` device.
    #[must_use]
    pub fn read() -> Option<String> {
        Self::read_raw().map(|(s, ..)| s)
    }

    /// Same as [`Self::read`] but also hands back the bare percentage --
    /// see [`crate::volume::Volume::sink_raw`] for why.
    #[must_use]
    pub fn read_raw() -> Option<(String, u32)> {
        let dir = fs::read_dir("/sys/class/backlight").ok()?;
        for entry in dir.flatten() {
            let path = entry.path();
            let cur: f64 = path.join("brightness").read_trim()?.parse().ok()?;
            let max: f64 = path.join("max_brightness").read_trim()?.parse().ok()?;
            if max <= 0.0 {
                continue;
            }
            // Float source, no lossless-widening path available (see the
            // font crate's raster.rs doc for the full float<->int cast
            // rationale) -- `cur/max` is a fraction of a sysfs-reported
            // brightness, always non-negative and comfortably under u32::MAX
            // once scaled to a percentage.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pct = (cur / max * 100.0).round() as u32;
            return Some((Self::format(pct), pct));
        }
        None
    }

    /// Render a brightness percentage the same way [`Self::read`] does --
    /// see [`crate::volume::Volume::format`] for why this is `pub`.
    #[must_use]
    pub fn format(pct: u32) -> String {
        format!("{pct:>3}% {ICON}")
    }

    /// Nudge screen backlight brightness by `delta_pct` percentage points
    /// (negative to dim), via `logind`'s `Session.SetBrightness` D-Bus call
    /// (`dbus::logind::LogindSession::set_backlight_brightness`) rather than
    /// a direct `/sys/class/backlight/*/brightness` write.
    ///
    /// A direct sysfs write only works unprivileged when a udev rule has
    /// granted the `video` group (or an ACL/uaccess tag) write access to
    /// that file -- on a stock install without that rule (as on this
    /// machine: the `video` group has no regular users in it, only
    /// `greeter`), the write silently fails with a permission error that a
    /// bare `fs::write` swallows via `let _ =`. `logind` is the actual
    /// sanctioned unprivileged path: it's polkit-gated instead of
    /// file-permission-gated, and the default polkit policy allows an
    /// active local session to call it without a password prompt. Not
    /// `brightnessctl` either: its relative-delta syntax is asymmetric
    /// (`+N%` to raise but `N%-`, not `-N%`, to lower) and got this wrong in
    /// an earlier version, silently no-opping every dim adjustment.
    pub fn adjust(delta_pct: i32) {
        let Ok(dir) = fs::read_dir("/sys/class/backlight") else {
            return;
        };
        for entry in dir.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(cur) = path
                .join("brightness")
                .read_trim()
                .and_then(|s| s.parse::<i64>().ok())
            else {
                continue;
            };
            let Some(max) = path
                .join("max_brightness")
                .read_trim()
                .and_then(|s| s.parse::<i64>().ok())
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
                // Clamped to `0..=max` above (and `max > 0`, checked), so
                // this is always non-negative.
                let new_val_u32 = u32::try_from(new_val).unwrap_or(0);
                let _ = dbus::logind::LogindSession::set_backlight_brightness(
                    &mut conn,
                    name,
                    new_val_u32,
                );
            }
            return;
        }
    }
}
