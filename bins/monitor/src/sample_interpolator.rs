//! Bridges low-cost 1 Hz sample collection to fluid 60 Hz on-screen motion.
//!
//! Keeping this as a dedicated owner makes presentation interpolation explicit
//! rather than accidentally turning rendering cadence into system-I/O cadence.
//! It lives in the library face (not `main.rs`) for two reasons: it is
//! window/thread-independent (pure `MonitorSample` math over a clock), and its
//! hot path runs on *every* 60 Hz tick -- including the ~80% that skip paint --
//! so its per-tick allocation behaviour must be measurable by the `divan`
//! `AllocProfiler` benches, which can only reach library code.
//!
//! The central performance contract: [`SampleInterpolator::blend_into`] must
//! not touch the heap on the steady-state tick. An earlier version returned an
//! owned `MonitorSample` per call, which cloned four device-name `String`s
//! (`thermal_device`, `network_device`, `cpu_brand`, `memory_device`) and
//! allocated a fresh `cores` `Vec` every single tick -- ~300 allocations a
//! second producing byte-identical output during the glide's hold phase. That
//! was the entire cost of raising the tick rate from 20 to 60 Hz. `blend_into`
//! writes into a caller-owned buffer instead: it reuses the `cores` capacity
//! and only rewrites a device string when its identity actually changed
//! (hotplug / first sample), so the hold path allocates nothing.

use std::time::{Duration, Instant};
use sysmon::MonitorSample;

/// How long a fresh sample takes to glide to its new value, independent of the
/// 1 Hz sampling cadence.
///
/// Deliberately shorter than the sample interval: the interpolator front-loads
/// the motion into this window and then *holds* the endpoint for the remainder
/// of the second. Holding is what lets the redraw gate skip -- once the glide
/// settles, the quantized display stops changing and paints/presents drop to
/// zero until the next sample. ~450ms reads as smooth to the eye while
/// reclaiming the majority of frames.
pub const GLIDE_DURATION: Duration = Duration::from_millis(450);

/// Interpolates between the previous sample (`from`) and the newest one (`to`)
/// as eased time elapses, so 1 Hz telemetry renders as continuous motion.
pub struct SampleInterpolator {
    from: MonitorSample,
    to: MonitorSample,
    started: Instant,
}

impl Default for SampleInterpolator {
    fn default() -> Self {
        Self::new()
    }
}

impl SampleInterpolator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            from: MonitorSample::default(),
            to: MonitorSample::default(),
            started: Instant::now(),
        }
    }

    /// Accept a freshly collected sample as the new glide target. Runs at the
    /// 1 Hz collection cadence, so the owned `value()` snapshot it takes for
    /// the new `from` (which does allocate) is off the hot path -- the
    /// per-tick cost lives entirely in [`Self::blend_into`].
    pub fn accept(&mut self, next: MonitorSample) {
        self.from = self.value();
        self.to = next;
        self.started = Instant::now();
    }

    /// The current interpolated sample as an owned value. Used by `accept`
    /// (1 Hz) and tests; the render loop uses [`Self::blend_into`] to avoid
    /// the per-tick allocation this owned form necessarily pays.
    #[must_use]
    pub fn value(&self) -> MonitorSample {
        let mut out = self.to.clone();
        self.blend_into(&mut out);
        out
    }

    /// Write the current interpolated sample into `out`, reusing its existing
    /// allocations. This is the hot path -- called once per 60 Hz tick,
    /// including on skipped frames -- and must not allocate in steady state.
    ///
    /// `out` is expected to already carry the target sample's device-name
    /// strings (the render loop passes the buffer that most recently held a
    /// real sample); the `clone_from`-under-guard below repairs them only when
    /// a device identity genuinely changed, so the common tick touches no heap.
    pub fn blend_into(&self, out: &mut MonitorSample) {
        let t = self.progress();
        let lerp = |a: f32, b: f32| (b - a).mul_add(t, a);

        // Reuse the buffer's `cores` capacity: clear keeps the allocation, and
        // `extend` refills it without a new one once it has grown to core count.
        out.cores.clear();
        out.cores
            .extend(self.to.cores.iter().enumerate().map(|(index, &value)| {
                lerp(self.from.cores.get(index).copied().unwrap_or(value), value)
            }));

        out.cpu = lerp(self.from.cpu, self.to.cpu);
        out.load = std::array::from_fn(|index| lerp(self.from.load[index], self.to.load[index]));
        out.memory = lerp(self.from.memory, self.to.memory);
        out.temperature = lerp(self.from.temperature, self.to.temperature);
        out.thermal_max = lerp(self.from.thermal_max, self.to.thermal_max);
        out.thermal_avg = lerp(self.from.thermal_avg, self.to.thermal_avg);
        out.thermal_min = lerp(self.from.thermal_min, self.to.thermal_min);
        out.disk = lerp(self.from.disk, self.to.disk);
        out.network_rx = lerp(self.from.network_rx, self.to.network_rx);
        out.network_tx = lerp(self.from.network_tx, self.to.network_tx);
        out.network_ping_ms = lerp(self.from.network_ping_ms, self.to.network_ping_ms);
        out.network_link_mbps = lerp(self.from.network_link_mbps, self.to.network_link_mbps);
        out.cpu_frequency_ghz = lerp(self.from.cpu_frequency_ghz, self.to.cpu_frequency_ghz);
        out.disk_io_mb_s = lerp(self.from.disk_io_mb_s, self.to.disk_io_mb_s);
        out.memory_available_mb = lerp(self.from.memory_available_mb, self.to.memory_available_mb);
        out.swap_used_mb = lerp(self.from.swap_used_mb, self.to.swap_used_mb);
        out.swap_total_mb = lerp(self.from.swap_total_mb, self.to.swap_total_mb);
        out.cpu_cores = self.to.cpu_cores;

        // Device-name strings are constant between samples. `clone_from` reuses
        // `out`'s existing buffer, and the `!=` guard means the steady-state
        // tick -- where nothing changed -- performs only cheap comparisons and
        // never allocates. A device hotplug or the first real sample pays the
        // clone exactly once.
        Self::sync_string(&mut out.thermal_device, &self.to.thermal_device);
        Self::sync_string(&mut out.network_device, &self.to.network_device);
        Self::sync_string(&mut out.cpu_brand, &self.to.cpu_brand);
        Self::sync_string(&mut out.memory_device, &self.to.memory_device);
    }

    /// Copy `src` into `dst` only when they differ, reusing `dst`'s allocation
    /// via `clone_from`. Comparing first keeps the unchanged hot path off the
    /// heap; the comparison is a short device-name strcmp, negligible per tick.
    fn sync_string(dst: &mut String, src: &str) {
        if dst != src {
            dst.clear();
            dst.push_str(src);
        }
    }

    /// Eased 0..=1 glide progress. Linear time is mapped through a cubic
    /// ease-out so the motion is quick at the start and decelerates into its
    /// endpoint -- this is what makes a 450ms glide read as smooth as the old
    /// 1s linear one despite finishing in less than half the time. Crucially
    /// it reaches *exactly* 1.0 at `GLIDE_DURATION` and stays there, so the
    /// interpolator holds a stable endpoint for the rest of the sample second
    /// and the redraw gate can skip every frame until the next sample.
    fn progress(&self) -> f32 {
        Self::eased_progress(self.started.elapsed(), GLIDE_DURATION)
    }

    /// Pure easing: map `elapsed` within a `glide` window to eased 0..=1
    /// progress. Split out from `progress` so the settle behaviour (reaches
    /// exactly 1.0 at the window end and never overshoots) is provable by a
    /// plain unit test without controlling the clock -- the same testability
    /// pattern `RedrawGate` uses.
    #[must_use]
    fn eased_progress(elapsed: Duration, glide: Duration) -> f32 {
        let linear = (elapsed.as_secs_f32() / glide.as_secs_f32()).clamp(0.0, 1.0);
        // Cubic ease-out: 1 - (1 - t)^3. Monotonic, hits 0 at t=0 and 1 at t=1.
        let inv = 1.0 - linear;
        inv.mul_add(-inv * inv, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{GLIDE_DURATION, SampleInterpolator};
    use std::time::Duration;
    use sysmon::MonitorSample;

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the exact 0.0 endpoint IS the property under test: clamp(0.0, 1.0) \
                  then 1 - (1-0)^3 is exactly representable, so an approximate \
                  compare would pass on a curve that never truly starts at rest"
    )]
    fn glide_starts_at_zero_and_moves_immediately() {
        // t=0 must be exactly the source value (progress 0), and a small
        // elapsed must already show visible motion -- the ease-out is
        // front-loaded, so early progress is well ahead of linear.
        assert_eq!(
            SampleInterpolator::eased_progress(Duration::ZERO, GLIDE_DURATION),
            0.0
        );
        let quarter = SampleInterpolator::eased_progress(GLIDE_DURATION / 4, GLIDE_DURATION);
        assert!(
            quarter > 0.25,
            "ease-out should be ahead of linear early on, got {quarter}"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "settling on EXACTLY 1.0 is the entire point (see the doc comment \
                  on `eased_progress`): anything merely near 1.0 keeps the \
                  quantized display creeping and pins the redraw gate at 0% skips, \
                  which a tolerance-based assert would silently accept"
    )]
    fn glide_settles_exactly_at_window_end_and_holds() {
        // The whole point of the shorter window: progress reaches exactly 1.0
        // at GLIDE_DURATION and never overshoots past it, so the endpoint is
        // stable for the rest of the sample second and the redraw gate can
        // skip. Anything less than a hard 1.0 would keep the quantized display
        // creeping and pin the gate at 0% skips.
        assert_eq!(
            SampleInterpolator::eased_progress(GLIDE_DURATION, GLIDE_DURATION),
            1.0
        );
        let held = SampleInterpolator::eased_progress(Duration::from_secs(1), GLIDE_DURATION);
        assert_eq!(
            held, 1.0,
            "progress must hold at 1.0 after the glide window"
        );
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "final-step assert checks the same exact-1.0 settle property as \
                  `glide_settles_exactly_at_window_end_and_holds`"
    )]
    fn glide_is_monotonic_across_the_window() {
        // Ease-out must never move backwards; a non-monotonic curve would make
        // a hero number briefly reverse direction mid-glide.
        let mut last = 0.0;
        for step in 0..=20 {
            let elapsed = GLIDE_DURATION * step / 20;
            let p = SampleInterpolator::eased_progress(elapsed, GLIDE_DURATION);
            assert!(
                p >= last,
                "progress went backwards at step {step}: {p} < {last}"
            );
            last = p;
        }
        assert_eq!(last, 1.0);
    }

    #[test]
    fn blend_into_reuses_buffer_and_matches_owned_value() {
        // The allocation-free hot path must produce the same numbers as the
        // owned `value()` it replaces -- otherwise the optimization changed
        // what's on screen. `value()` and `blend_into` read the wall clock at
        // slightly different instants, so their eased `t` differs by a few
        // microseconds; compare with a tolerance that swamps that jitter while
        // still catching any real divergence in the blend math. Device names
        // are time-independent and compared exactly.
        let mut interp = SampleInterpolator::new();
        interp.accept(MonitorSample {
            cpu: 10.0,
            cores: vec![5.0, 15.0],
            thermal_device: "zone0".to_owned(),
            ..Default::default()
        });
        interp.accept(MonitorSample {
            cpu: 90.0,
            cores: vec![95.0, 85.0],
            thermal_device: "zone0".to_owned(),
            ..Default::default()
        });
        let owned = interp.value();
        let mut buffer = MonitorSample::default();
        interp.blend_into(&mut buffer);
        // Range here is 10..=90, so a 0.5 tolerance is ~0.5% -- far above the
        // sub-microsecond clock skew, far below any real math error.
        assert!(
            (buffer.cpu - owned.cpu).abs() < 0.5,
            "cpu diverged: {} vs {}",
            buffer.cpu,
            owned.cpu
        );
        assert_eq!(buffer.cores.len(), owned.cores.len());
        for (b, o) in buffer.cores.iter().zip(&owned.cores) {
            assert!((b - o).abs() < 0.5, "core diverged: {b} vs {o}");
        }
        assert_eq!(buffer.thermal_device, owned.thermal_device);
    }

    #[test]
    fn blend_into_syncs_a_changed_device_name() {
        // A device hotplug between samples must be reflected in the reused
        // buffer even though the guard skips unchanged names.
        let mut interp = SampleInterpolator::new();
        interp.accept(MonitorSample {
            network_device: "eth0".to_owned(),
            ..Default::default()
        });
        let mut buffer = MonitorSample::default();
        interp.blend_into(&mut buffer);
        assert_eq!(buffer.network_device, "eth0");
        interp.accept(MonitorSample {
            network_device: "wlan0".to_owned(),
            ..Default::default()
        });
        interp.blend_into(&mut buffer);
        assert_eq!(buffer.network_device, "wlan0");
    }
}
