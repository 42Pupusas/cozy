//! `monitor::sample_interpolator::SampleInterpolator::blend_into`: the 1 Hz ->
//! 60 Hz interpolation, run on *every* tick including the ~80% that skip paint.
//! The live 60 Hz trace showed CPU regressing 1.0% -> ~1.5% despite a 70-87%
//! skip rate, which pointed at per-tick allocation rather than paint. The
//! prior owned-return `value()` cloned four device-name `String`s and a
//! `cores` `Vec` every tick; `blend_into` writes into a reused buffer. This
//! bench measures both under `AllocProfiler` so the "hot path allocates
//! nothing in steady state" claim is a number, not an assertion.
//!
//! Run:
//!   cargo bench -p monitor-benches --bench blend

use monitor::sample_interpolator::SampleInterpolator;
use sysmon::MonitorSample;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

#[allow(clippy::cast_precision_loss)]
fn sample(cores: usize, thermal: &str, net: &str) -> MonitorSample {
    MonitorSample {
        cpu: 42.0,
        cores: (0..cores).map(|i| (i * 7 % 100) as f32).collect(),
        load: [1.2, 0.8, 0.5],
        memory: 60.0,
        temperature: 55.0,
        thermal_device: thermal.to_owned(),
        network_device: net.to_owned(),
        cpu_brand: "Test CPU @ 3.8GHz".to_owned(),
        memory_device: "DIMM0".to_owned(),
        ..Default::default()
    }
}

/// A primed interpolator mid-glide with stable device names -- the steady
/// state. `blend_into` here must report ZERO allocations: the cores Vec reuses
/// the buffer's capacity and every device string is unchanged so the guard
/// skips the clone. This is the number that decides whether 60Hz ticking fits
/// the CPU budget.
#[divan::bench]
fn blend_into_steady_state(bencher: divan::Bencher) {
    let mut interp = SampleInterpolator::new();
    interp.accept(sample(16, "zone0", "eth0"));
    interp.accept(sample(16, "zone0", "eth0"));
    // Pre-grow the destination buffer exactly as the render loop's persistent
    // sample buffer would be after the first real blend.
    let mut buffer = MonitorSample::default();
    interp.blend_into(&mut buffer);
    bencher.bench_local(|| {
        interp.blend_into(divan::black_box(&mut buffer));
        divan::black_box(&buffer);
    });
}

/// For contrast: the owned-value form the hot path used to call. Its
/// allocation count (device clones + cores Vec + the base clone) is the per-
/// tick cost that `blend_into` eliminates -- kept as a measured baseline so
/// the improvement is a ratio, not a claim.
#[divan::bench]
fn value_owned_baseline(bencher: divan::Bencher) {
    let mut interp = SampleInterpolator::new();
    interp.accept(sample(16, "zone0", "eth0"));
    interp.accept(sample(16, "zone0", "eth0"));
    bencher.bench_local(|| divan::black_box(interp.value()));
}
