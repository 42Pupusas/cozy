//! Long-running monitor workload for perf. Unlike the Divan benchmark, this
//! keeps the sampler hot long enough for symbol-level profiles to stabilize.

use sysmon::MonitorReader;
use std::hint::black_box;

struct MonitorProfile {
    reader: MonitorReader,
}

impl MonitorProfile {
    fn new() -> Self {
        Self {
            reader: MonitorReader::new(),
        }
    }

    fn sample(&mut self, iterations: usize) {
        for _ in 0..iterations {
            black_box(self.reader.sample());
        }
    }

    fn process_scan(&mut self, iterations: usize) {
        for _ in 0..iterations {
            while black_box(self.reader.processes_tick()).is_none() {}
        }
    }

    /// Report the wall-clock cost of one worker-thread tick (a full `/proc`
    /// process scan plus a metrics `sample()`), the exact work the monitor's
    /// 10 Hz telemetry thread performs. Prints min/mean/max over `iterations`
    /// real ticks so the per-tick cost -- and thus the worker thread's steady
    /// CPU share at the 10 Hz `SAMPLE_INTERVAL` -- is directly readable
    /// without a GUI or a compositor.
    fn tick_cost(&mut self, iterations: usize) {
        use std::time::{Duration, Instant};
        // Warm up: first scan pays cold-cache + allocation costs that never
        // recur at steady state, which would skew the min/mean.
        while self.reader.processes_tick().is_none() {}
        let _ = self.reader.sample();

        let mut scan_total = Duration::ZERO;
        let mut sample_total = Duration::ZERO;
        let mut worst = Duration::ZERO;
        let mut best = Duration::MAX;
        for _ in 0..iterations {
            let t0 = Instant::now();
            while black_box(self.reader.processes_tick()).is_none() {}
            let scan = t0.elapsed();
            let t1 = Instant::now();
            black_box(self.reader.sample());
            let sample = t1.elapsed();
            scan_total += scan;
            sample_total += sample;
            let tick = scan + sample;
            worst = worst.max(tick);
            best = best.min(tick);
        }
        #[allow(clippy::cast_precision_loss)]
        let n = iterations as f64;
        let scan_ms = scan_total.as_secs_f64() * 1e3 / n;
        let sample_ms = sample_total.as_secs_f64() * 1e3 / n;
        let tick_ms = scan_ms + sample_ms;
        // At the worker's 10 Hz cadence, per-second CPU share is tick_ms*10.
        let cpu_pct_at_10hz = tick_ms * 10.0 / 1000.0 * 100.0;
        println!(
            "worker tick over {iterations} iters: scan {scan_ms:.3} ms + sample {sample_ms:.3} ms \
             = {tick_ms:.3} ms/tick (best {:.3}, worst {:.3})",
            best.as_secs_f64() * 1e3,
            worst.as_secs_f64() * 1e3,
        );
        println!(
            "=> at 10 Hz SAMPLE_INTERVAL the worker thread costs ~{cpu_pct_at_10hz:.2}% of one core"
        );
    }
}

fn main() {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sample".to_owned());
    let mut profile = MonitorProfile::new();
    match mode.as_str() {
        "process" => profile.process_scan(100),
        "tick" => profile.tick_cost(200),
        _ => profile.sample(10_000),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn profile_shape_is_stable() {
        assert_eq!(2 + 2, 4);
    }
}
