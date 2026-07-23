//! Quantizes a [`sysmon::MonitorSample`] down to the precision the
//! dashboard actually renders, so the render loop can tell whether a new
//! interpolated value would produce different pixels at all.
//!
//! An earlier version of the redraw gate checked whether
//! `SampleInterpolator`'s raw float blend had "settled" (`t >= 1.0`). That
//! was the wrong signal: the blend is *designed* to run continuously for
//! the entire ~100ms gap between samples so hero numbers glide instead of
//! snapping, so it is essentially never settled during normal operation --
//! confirmed live via `MONITOR_PROFILE=1`, which showed a skip rate stuck
//! at 0-1.7% instead of the expected 85%+. The fix is to compare what the
//! dashboard actually *draws* (numbers rounded to their displayed decimal
//! place, core bars rounded to the nearest whole percent) rather than the
//! raw, continuously-moving interpolation state.

use sysmon::MonitorSample;

/// The rendered-precision projection of one [`MonitorSample`].
///
/// Every field here is quantized to match the `format!` precision used at its
/// one draw site in `Painter::paint` (see the module doc for why this matters
/// more than raw interpolation progress).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplaySnapshot {
    cpu_hero: i32,         // "{:.0}"
    cores: Vec<i32>,       // bars are rounded to whole percent height
    load0: i32,            // "{:.2}" -> compare at 1/100ths
    disk_io: i32,          // "{:.1}" -> tenths
    frequency: i32,        // "{:.2}" -> hundredths
    memory_hero: i32,      // "{:.0}"
    memory_available: i32, // "{:.1}" GB -> tenths
    swap_used: i32,        // "{:.1}" GB -> tenths
    temperature_hero: i32, // "{:.0}"
    thermal_max: i32,      // "{:.1}"
    thermal_avg: i32,      // "{:.1}"
    thermal_min: i32,      // "{:.1}"
    network_ping: i32,     // "{:.1}"
    network_rx: i32,       // "{:.2}"
    network_tx: i32,       // "{:.2}"
    network_link: i32,     // "{:.0}"
}

impl DisplaySnapshot {
    /// Quantizes `value` to `decimals` places by scaling and rounding to the
    /// nearest integer. Comparing these integers is equivalent to comparing
    /// the strings `format!("{value:.decimals$}")` would produce, without
    /// paying for string allocation on every frame.
    ///
    /// This runs on *every* redraw-gate tick, including skipped ones, so at
    /// 60Hz it executes ~16 times per frame, 60 times a second. It must not
    /// allocate. An earlier version routed through `.to_string().parse()`
    /// purely to saturate cleanly on a non-finite/out-of-range source; that
    /// cost a heap `String` per field (measured: 32 allocs/tick on a 16-core
    /// box via the `idle_tick` bench). `f32 as i32` already saturates at the
    /// type bounds and yields 0 for NaN in Rust, so a plain checked cast gives
    /// identical saturation behaviour with zero allocation -- the property
    /// that makes a 60Hz idle tick affordable.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "the saturating f32->i32 cast IS the intended quantization/clamp; see doc above"
    )]
    fn quantize(value: f32, decimals: i32) -> i32 {
        let scale = 10f32.powi(decimals);
        // `as i32` is a saturating cast in Rust: values above i32::MAX clamp to
        // i32::MAX, below i32::MIN to i32::MIN, and NaN maps to 0. That matches
        // the previous parse-based saturation for all finite telemetry while
        // never touching the heap.
        (value * scale).round() as i32
    }

    #[must_use]
    pub fn from_sample(sample: &MonitorSample) -> Self {
        Self {
            cpu_hero: Self::quantize(sample.cpu, 0),
            cores: sample.cores.iter().map(|&c| Self::quantize(c, 0)).collect(),
            load0: Self::quantize(sample.load[0], 2),
            disk_io: Self::quantize(sample.disk_io_mb_s, 1),
            frequency: Self::quantize(sample.cpu_frequency_ghz, 2),
            memory_hero: Self::quantize(sample.memory, 0),
            memory_available: Self::quantize(sample.memory_available_mb / 1024.0, 1),
            swap_used: Self::quantize(sample.swap_used_mb / 1024.0, 1),
            temperature_hero: Self::quantize(sample.temperature, 0),
            thermal_max: Self::quantize(sample.thermal_max, 1),
            thermal_avg: Self::quantize(sample.thermal_avg, 1),
            thermal_min: Self::quantize(sample.thermal_min, 1),
            network_ping: Self::quantize(sample.network_ping_ms, 1),
            network_rx: Self::quantize(sample.network_rx, 2),
            network_tx: Self::quantize(sample.network_tx, 2),
            network_link: Self::quantize(sample.network_link_mbps, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DisplaySnapshot;
    use sysmon::MonitorSample;

    fn sample_with_cpu(cpu: f32) -> MonitorSample {
        MonitorSample {
            cpu,
            ..Default::default()
        }
    }

    #[test]
    fn sub_display_precision_jitter_is_not_a_change() {
        // This is the actual noise floor the render loop was fighting: raw
        // interpolation moves the float every frame even when the rounded
        // "{:.0}" text drawn on screen is identical.
        let a = DisplaySnapshot::from_sample(&sample_with_cpu(42.31));
        let b = DisplaySnapshot::from_sample(&sample_with_cpu(42.49));
        assert_eq!(a, b, "both round to the same displayed \"42\"");
    }

    #[test]
    fn crossing_a_displayed_integer_boundary_is_a_change() {
        let a = DisplaySnapshot::from_sample(&sample_with_cpu(42.49));
        let b = DisplaySnapshot::from_sample(&sample_with_cpu(42.51));
        assert_ne!(a, b, "42.49 displays as 42, 42.51 displays as 43");
    }

    #[test]
    fn core_bar_percentages_quantize_to_whole_percent() {
        let a = DisplaySnapshot::from_sample(&MonitorSample {
            cores: vec![10.1, 20.2],
            ..Default::default()
        });
        let b = DisplaySnapshot::from_sample(&MonitorSample {
            cores: vec![10.4, 20.4],
            ..Default::default()
        });
        assert_eq!(a, b, "sub-percent core jitter shouldn't move a bar");
    }

    #[test]
    fn load_average_within_the_same_hundredth_is_not_a_change() {
        let a = DisplaySnapshot::from_sample(&MonitorSample {
            load: [1.234, 0.0, 0.0],
            ..Default::default()
        });
        let b = DisplaySnapshot::from_sample(&MonitorSample {
            load: [1.2344, 0.0, 0.0],
            ..Default::default()
        });
        assert_eq!(a, b, "both round to the displayed \"1.23\"");
    }

    #[test]
    fn non_finite_and_extreme_values_saturate_without_panicking() {
        // The allocation-free `quantize` relies on `f32 as i32` saturation to
        // replace the old `to_string().parse()` guard. Prove the edge cases
        // the guard used to cover: NaN and out-of-range magnitudes must not
        // panic and must land at stable clamped values, so a garbage telemetry
        // reading can never crash the render loop or thrash the redraw gate.
        let nan = DisplaySnapshot::from_sample(&sample_with_cpu(f32::NAN));
        assert_eq!(nan, DisplaySnapshot::from_sample(&sample_with_cpu(f32::NAN)));
        let huge = DisplaySnapshot::from_sample(&sample_with_cpu(f32::INFINITY));
        let huge2 = DisplaySnapshot::from_sample(&sample_with_cpu(1e30));
        assert_eq!(huge, huge2, "both saturate to the same clamped hero value");
    }

    #[test]
    fn load_average_crossing_a_hundredth_boundary_is_a_change() {
        let a = DisplaySnapshot::from_sample(&MonitorSample {
            load: [1.234, 0.0, 0.0],
            ..Default::default()
        });
        let b = DisplaySnapshot::from_sample(&MonitorSample {
            load: [1.236, 0.0, 0.0],
            ..Default::default()
        });
        assert_ne!(
            a, b,
            "1.234 displays as \"1.23\", 1.236 displays as \"1.24\""
        );
    }
}
