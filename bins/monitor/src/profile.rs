//! Opt-in, thread-level monitor profiling.
//!
//! Wall-clock paint timing alone cannot explain process CPU use: it misses
//! presentation and cannot distinguish the Wayland thread from the sampler.
//! Linux's per-thread CPU clock reports time actually scheduled on a core,
//! which makes the two owners independently measurable.

use std::os::raw::c_int;
use std::time::{Duration, Instant};

/// Linux `clockid_t` used by `clock_gettime` for the calling thread's consumed
/// CPU time. This is intentionally not process CPU time: the monitor has a UI
/// thread and a worker, and we need to identify which owner is expensive.
const CLOCK_THREAD_CPUTIME_ID: c_int = 3;

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanoseconds: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}

struct ThreadClock;

impl ThreadClock {
    fn now() -> Duration {
        let mut time = Timespec {
            seconds: 0,
            nanoseconds: 0,
        };
        // SAFETY: `time` is valid writable storage and the clock id is the
        // Linux-defined current-thread CPU clock. Falling back to zero keeps
        // instrumentation optional if a non-Linux runtime rejects it.
        if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &raw mut time) } != 0 {
            return Duration::ZERO;
        }
        let seconds = u64::try_from(time.seconds).unwrap_or(0);
        let nanoseconds = u32::try_from(time.nanoseconds).unwrap_or(0);
        Duration::new(seconds, nanoseconds)
    }
}

/// Per-frame paint phases. The dashboard has two unusually different chart
/// workloads: the CPU card layers every core, while other cards draw one line.
/// Keep their timings separate so a core overlay optimization is evidence-led.
#[derive(Default)]
pub struct PaintPhases {
    pub cpu_chart: Duration,
    pub other_charts: Duration,
    pub labels: Duration,
    pub static_content: Duration,
    /// How many chart cards this frame returned *non-empty* damage -- i.e.
    /// actually scrolled/rebuilt and blitted, rather than hitting the idle
    /// early-return. The live trace shows `charts` dominating paint even
    /// though knots arrive only ~4/sec, which is impossible if idle charts
    /// are free; this counter measures whether they truly are, ending the
    /// model-vs-measurement contradiction instead of guessing at it.
    pub charts_active: u32,
}

/// Aggregates one-second costs for the Wayland/render thread.
pub struct FrameProfile {
    enabled: bool,
    started: Instant,
    cpu_started: Duration,
    frames: u32,
    full_frames: u32,
    paint_time: Duration,
    frame_time: Duration,
    cpu_chart_time: Duration,
    other_chart_time: Duration,
    label_time: Duration,
    static_time: Duration,
    /// Ticks where `RedrawGate` decided nothing changed and paint/present
    /// were skipped entirely. Tracked separately from `frames` (painted
    /// ticks) so the reported line makes the skip rate -- the entire point
    /// of the redraw-gating change -- directly visible without needing a
    /// live compositor to eyeball fps against.
    skipped_ticks: u32,
    /// Sum over painted frames of the fraction of window *rows* each frame's
    /// damage actually forced uploaded. The shipped shm path is row-band
    /// granular -- it premultiplies and copies every full-width row a damage
    /// rect touches, ignoring the rect's x/width -- so this row coverage, not
    /// the rect area, is what determines present cost. Averaged per frame in
    /// the report, it answers whether the present bill is spread across many
    /// vertically-separated chart bands (reducible) or already minimal.
    damage_row_fraction: f64,
    /// Sum over painted frames of how many chart cards were active (non-empty
    /// damage). Divided by frames in the report to show the average number of
    /// charts doing real copy work per painted frame -- the direct test of
    /// whether the idle-chart skip is actually firing in the live loop.
    charts_active: u64,
}

impl FrameProfile {
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: std::env::var_os("MONITOR_PROFILE").is_some(),
            started: Instant::now(),
            cpu_started: ThreadClock::now(),
            frames: 0,
            full_frames: 0,
            paint_time: Duration::ZERO,
            frame_time: Duration::ZERO,
            cpu_chart_time: Duration::ZERO,
            other_chart_time: Duration::ZERO,
            label_time: Duration::ZERO,
            static_time: Duration::ZERO,
            skipped_ticks: 0,
            damage_row_fraction: 0.0,
            charts_active: 0,
        }
    }

    /// Notes a tick where `RedrawGate` skipped paint entirely. Called from
    /// `App::run`'s skip branch so the reported skip rate reflects reality
    /// rather than being inferred from fps alone (which can't distinguish
    /// "compositor throttled us" from "we chose not to present").
    pub const fn record_skip(&mut self) {
        if self.enabled {
            self.skipped_ticks += 1;
        }
    }

    pub fn record(
        &mut self,
        paint_time: Duration,
        frame_time: Duration,
        phases: &PaintPhases,
        full_frame: bool,
        damage_row_fraction: f64,
    ) {
        if !self.enabled {
            return;
        }
        self.frames += 1;
        self.full_frames += u32::from(full_frame);
        self.paint_time += paint_time;
        self.frame_time += frame_time;
        self.damage_row_fraction += damage_row_fraction;
        self.cpu_chart_time += phases.cpu_chart;
        self.other_chart_time += phases.other_charts;
        self.label_time += phases.labels;
        self.static_time += phases.static_content;
        let elapsed = self.started.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let frames = f64::from(self.frames.max(1));
        let cpu_percent = ThreadClock::now()
            .saturating_sub(self.cpu_started)
            .as_secs_f64()
            / elapsed.as_secs_f64()
            * 100.0;
        let total_ticks = self.frames + self.skipped_ticks;
        let skip_percent = if total_ticks == 0 {
            0.0
        } else {
            f64::from(self.skipped_ticks) / f64::from(total_ticks) * 100.0
        };
        eprintln!(
            "monitor ui: {:.1} fps | {cpu_percent:.1}% cpu | {:.2} ms frame | {:.2} ms paint [cpu {:.2}, charts {:.2}, labels {:.2}, static {:.2}] | {:.1} charts/f | {:.0}% rows up | {}/{} full | {}/{} ticks skipped ({skip_percent:.1}%)",
            frames / elapsed.as_secs_f64(),
            self.frame_time.as_secs_f64() * 1_000.0 / frames,
            self.paint_time.as_secs_f64() * 1_000.0 / frames,
            self.cpu_chart_time.as_secs_f64() * 1_000.0 / frames,
            self.other_chart_time.as_secs_f64() * 1_000.0 / frames,
            self.label_time.as_secs_f64() * 1_000.0 / frames,
            self.static_time.as_secs_f64() * 1_000.0 / frames,
            {
                // charts_active is a small per-second count; the f64 cast is
                // exact far below the mantissa limit. Annotated locally rather
                // than widening the cast, to keep the intent obvious.
                #[allow(clippy::cast_precision_loss)]
                let active = self.charts_active as f64;
                active / frames
            },
            self.damage_row_fraction / frames * 100.0,
            self.full_frames,
            self.frames,
            self.skipped_ticks,
            total_ticks,
        );
        self.started = Instant::now();
        self.cpu_started = ThreadClock::now();
        self.frames = 0;
        self.full_frames = 0;
        self.paint_time = Duration::ZERO;
        self.frame_time = Duration::ZERO;
        self.cpu_chart_time = Duration::ZERO;
        self.other_chart_time = Duration::ZERO;
        self.label_time = Duration::ZERO;
        self.static_time = Duration::ZERO;
        self.skipped_ticks = 0;
        self.damage_row_fraction = 0.0;
        self.charts_active = 0;
    }
}

/// Separately reports sampler and `/proc` scan cost from the monitor worker.
pub struct WorkerProfile {
    enabled: bool,
    started: Instant,
    cpu_started: Duration,
    samples: u32,
    sample_time: Duration,
    process_scans: u32,
    process_time: Duration,
}

impl WorkerProfile {
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: std::env::var_os("MONITOR_PROFILE").is_some(),
            started: Instant::now(),
            cpu_started: ThreadClock::now(),
            samples: 0,
            sample_time: Duration::ZERO,
            process_scans: 0,
            process_time: Duration::ZERO,
        }
    }

    pub fn record_sample(&mut self, elapsed: Duration) {
        if self.enabled {
            self.samples += 1;
            self.sample_time += elapsed;
        }
    }

    pub fn record_process_scan(&mut self, elapsed: Duration) {
        if self.enabled {
            self.process_scans += 1;
            self.process_time += elapsed;
        }
    }

    pub fn report(&mut self) {
        if !self.enabled || self.started.elapsed() < Duration::from_secs(1) {
            return;
        }
        let elapsed = self.started.elapsed();
        let cpu_percent = ThreadClock::now()
            .saturating_sub(self.cpu_started)
            .as_secs_f64()
            / elapsed.as_secs_f64()
            * 100.0;
        let samples = f64::from(self.samples.max(1));
        eprintln!(
            "monitor worker: {cpu_percent:.1}% cpu | {:.2} ms/sample | {} scans, {:.2} ms/scan",
            self.sample_time.as_secs_f64() * 1_000.0 / samples,
            self.process_scans,
            if self.process_scans == 0 {
                0.0
            } else {
                self.process_time.as_secs_f64() * 1_000.0 / f64::from(self.process_scans)
            },
        );
        self.started = Instant::now();
        self.cpu_started = ThreadClock::now();
        self.samples = 0;
        self.sample_time = Duration::ZERO;
        self.process_scans = 0;
        self.process_time = Duration::ZERO;
    }
}
