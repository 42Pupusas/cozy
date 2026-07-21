//! `statusbar::ModuleKind::color_for`: per-module color lookup, called once
//! per segment on every repaint (so up to ~11 times per frame in `paint`).
//!
//! Dominated by the `Battery` variant's trailing-percent string split/parse
//! -- the other variants are O(1) glyph-prefix/suffix checks or a flat
//! match. Benchmarked separately so a future change to the string parsing
//! doesn't silently regress the whole paint loop unnoticed.
//!
//! Run:
//!   cargo bench -p statusbar-benches --bench colors

use statusbar::ModuleKind;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

const BATTERY_SAMPLE: &str = " 42% \u{f007a}";
const WIFI_SAMPLE: &str = "myssid \u{f1eb}";
const CLOCK_SAMPLE: &str = "12:34:56";

#[divan::bench]
fn battery_color(bencher: divan::Bencher) {
    bencher.bench(|| ModuleKind::Battery.color_for(divan::black_box(BATTERY_SAMPLE)));
}

#[divan::bench]
fn wifi_color(bencher: divan::Bencher) {
    bencher.bench(|| ModuleKind::Wifi.color_for(divan::black_box(WIFI_SAMPLE)));
}

#[divan::bench]
fn clock_color(bencher: divan::Bencher) {
    bencher.bench(|| ModuleKind::Clock.color_for(divan::black_box(CLOCK_SAMPLE)));
}

/// One full `paint()`-equivalent pass: every module's color resolved once,
/// matching the real per-frame call pattern more closely than any single
/// variant in isolation.
#[divan::bench]
fn full_segment_set(bencher: divan::Bencher) {
    let segments = [
        (ModuleKind::Clock, CLOCK_SAMPLE),
        (ModuleKind::Date, "2024-01-01"),
        (ModuleKind::Wifi, WIFI_SAMPLE),
        (ModuleKind::Bluetooth, "\u{f00af}"),
        (ModuleKind::Battery, BATTERY_SAMPLE),
        (ModuleKind::Temp, " 55\u{b0}C"),
        (ModuleKind::VolOut, " 50% \u{f057e}"),
        (ModuleKind::VolIn, " 50% \u{f036c}"),
        (ModuleKind::Brightness, " 80%"),
    ];
    bencher.bench(|| {
        for (kind, text) in divan::black_box(segments) {
            let _ = kind.color_for(text);
        }
    });
}
