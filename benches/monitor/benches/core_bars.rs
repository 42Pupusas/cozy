//! `monitor::core_bars::CoreBars::paint`: the per-core CPU bars. `perf record`
//! once found this widget's unconditional full-background `fill_rect` as the
//! single dominant symbol in `Painter::paint` (the ~0.95ms -> ~9ms live jump).
//! It was rewritten to repaint only columns whose bar height changed. This
//! bench guards that: the "all changed" regime is the worst case (every bar
//! moved), and the "none changed" regime must be near-free with zero
//! allocations -- the property that stopped it dominating the frame.
//!
//! Run:
//!   cargo bench -p monitor-benches --bench core_bars

#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use monitor::core_bars::{CoreBarPaint, CoreBars};
use platform::draw::{Canvas, Color, Rect};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

const CORES: usize = 16;
const BARS_W: i32 = 360;
const BARS_H: i32 = 96;

const COLORS: &[Color] = &[
    Color::rgb(0x3a, 0xd0, 0x7a),
    Color::rgb(0x3a, 0x9a, 0xd0),
    Color::rgb(0xd0, 0x9a, 0x3a),
    Color::rgb(0xd0, 0x3a, 0x7a),
];

fn paint_config() -> CoreBarPaint<'static> {
    CoreBarPaint {
        bounds: Rect::new(0, 0, BARS_W, BARS_H),
        background: Color::rgb(0x12, 0x14, 0x18),
        colors: COLORS,
        background_cleared: false,
    }
}

/// Worst case: every core's utilization changed since last paint, so every
/// column is repainted. This is the ceiling the widget can cost per frame.
#[divan::bench]
fn all_changed(bencher: divan::Bencher) {
    let mut bars = CoreBars::new();
    let mut target = vec![0u32; (BARS_W * BARS_H) as usize];
    // Prime once so `last_heights` is populated.
    {
        let mut canvas = Canvas::new(&mut target, BARS_W, BARS_H);
        let cores = vec![0.0f32; CORES];
        bars.paint(&mut canvas, &cores, paint_config());
    }
    let mut tick = 0u32;
    bencher.bench_local(|| {
        tick += 1;
        // Every core gets a distinct new value each iteration -> all columns
        // differ from last_heights.
        let cores: Vec<f32> = (0..CORES)
            .map(|i| ((tick as usize + i) * 7 % 100) as f32)
            .collect();
        let mut canvas = Canvas::new(&mut target, BARS_W, BARS_H);
        divan::black_box(bars.paint(&mut canvas, divan::black_box(&cores), paint_config()));
    });
}

/// Steady state: no core's quantized height changed, so nothing is repainted.
/// Must be near-free and allocation-free -- the property that took this widget
/// off the frame's critical path.
#[divan::bench]
fn none_changed(bencher: divan::Bencher) {
    let mut bars = CoreBars::new();
    let mut target = vec![0u32; (BARS_W * BARS_H) as usize];
    let cores = vec![42.0f32; CORES];
    {
        let mut canvas = Canvas::new(&mut target, BARS_W, BARS_H);
        bars.paint(&mut canvas, &cores, paint_config());
    }
    bencher.bench_local(|| {
        let mut canvas = Canvas::new(&mut target, BARS_W, BARS_H);
        divan::black_box(bars.paint(&mut canvas, divan::black_box(&cores), paint_config()));
    });
}
