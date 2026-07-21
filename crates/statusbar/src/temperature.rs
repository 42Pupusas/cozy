//! CPU package/core temperature, from `/sys/class/thermal`.
//!
//! A marker type (like [`crate::volume::Volume`]) rather than a value: every
//! call is a fresh sysfs read, there's no state to hold between polls.

use crate::sys_file::ReadTrimmed;
use std::fs;

/// Temperature reads -- see the module doc for why this is a marker type.
pub struct Temperature;

impl Temperature {
    /// CPU package/core temperature in °C, from the first
    /// `/sys/class/thermal/thermal_zone*/temp` that parses (millidegrees).
    #[must_use]
    pub fn read() -> Option<String> {
        let dir = fs::read_dir("/sys/class/thermal").ok()?;
        for entry in dir.flatten() {
            let path = entry.path().join("temp");
            if let Some(raw) = path.read_trim()
                && let Ok(milli) = raw.parse::<i64>()
            {
                // A real thermal-zone millidegree reading is nowhere
                // near f64's 52-bit mantissa limit, but that bound isn't
                // provable from the i64 type alone -- same situation as
                // the font crate's documented float casts.
                #[allow(clippy::cast_precision_loss)]
                let celsius = milli as f64 / 1000.0;
                return Some(format!("{celsius:>3.0}°C"));
            }
        }
        None
    }
}
