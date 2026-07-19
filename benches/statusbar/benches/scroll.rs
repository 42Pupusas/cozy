//! `statusbar::ScrollAccumulator`: the wheel-scroll debounce/accumulation
//! math that runs on every `wl_pointer.axis` event while the pointer is
//! hovering a scrollable segment.
//!
//! This is cheap by construction (a handful of integer ops), so the
//! interesting number here isn't wall-time -- it's the allocation
//! tripwire: this type must be alloc-free on its hot path (it's plain
//! `Copy` integer state), same posture as `tty-benches`' `parser`/
//! `pipeline` benches for `vt_parser`/`grid`.
//!
//! Run:
//!   cargo bench -p statusbar-benches --bench scroll

use statusbar::ScrollAccumulator;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Simulates one physical kinetic-scroll gesture's worth of small axis
/// events -- the shape that made this bar's scrolling feel janky before
/// the accumulator existed (see `scroll.rs`'s module doc).
#[divan::bench]
fn kinetic_gesture_burst(bencher: divan::Bencher) {
    bencher.bench(|| {
        let mut acc = ScrollAccumulator::new();
        let mut steps = 0;
        for _ in 0..divan::black_box(60) {
            acc.add(divan::black_box(2));
            steps += acc.take_steps();
        }
        steps
    });
}

#[divan::bench]
fn single_detent_add_and_take(bencher: divan::Bencher) {
    bencher.bench(|| {
        let mut acc = ScrollAccumulator::new();
        acc.add(divan::black_box(15));
        acc.take_steps()
    });
}
