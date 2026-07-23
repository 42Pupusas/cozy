//! `monitor::display_snapshot::DisplaySnapshot::from_sample`: the redraw
//! gate's input, computed on *every* frame tick -- including the ones that
//! then skip paint entirely. It is the price of asking "would this frame draw
//! a different pixel than the last?". At 20Hz that price was invisible; the
//! goal now is 60Hz presentation under 1% CPU, so this per-tick cost runs
//! 60x/sec and its allocations dominate the skip path. This bench exists to
//! measure that -- with `AllocProfiler` -- because the suspected cost is a
//! `String` round-trip (`.round().to_string().parse()`) buried in `quantize`
//! plus a per-tick `Vec` for the core list, and hand-reasoning about it is
//! exactly what got us the wrong signal before.
//!
//! Run:
//!   cargo bench -p monitor-benches --bench idle_tick

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]

use monitor::display_snapshot::DisplaySnapshot;
use statusbar::MonitorSample;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// A representative sample: a real core count and non-trivial values in every
/// quantized field, so `quantize` runs its full path per field rather than
/// short-cutting on zeros.
fn sample(cores: usize) -> MonitorSample {
    MonitorSample {
        cpu: 42.37,
        cores: (0..cores).map(|i| (i * 7 % 100) as f32 + 0.3).collect(),
        load: [1.234, 0.87, 0.55],
        memory: 63.4,
        temperature: 54.6,
        thermal_max: 61.2,
        thermal_avg: 55.5,
        thermal_min: 48.1,
        disk: 12.0,
        network_rx: 3.14,
        network_tx: 2.71,
        network_ping_ms: 18.4,
        network_link_mbps: 940.0,
        cpu_frequency_ghz: 3.87,
        disk_io_mb_s: 22.6,
        memory_available_mb: 8192.0,
        swap_used_mb: 512.0,
        swap_total_mb: 8192.0,
        ..Default::default()
    }
}

/// The full gate input for a typical 16-core machine, built once per tick.
/// Under `AllocProfiler` this is the number that decides whether 60Hz idle
/// ticks fit the CPU budget: every allocation here is paid 60x/sec on frames
/// that ultimately draw nothing.
#[divan::bench]
fn from_sample_16_cores(bencher: divan::Bencher) {
    let sample = sample(16);
    bencher.bench_local(|| divan::black_box(DisplaySnapshot::from_sample(divan::black_box(&sample))));
}

/// A smaller machine, to see how much of the cost scales with core count (the
/// `cores` Vec + per-core quantize) versus the fixed ~16 scalar fields.
#[divan::bench]
fn from_sample_4_cores(bencher: divan::Bencher) {
    let sample = sample(4);
    bencher.bench_local(|| divan::black_box(DisplaySnapshot::from_sample(divan::black_box(&sample))));
}
