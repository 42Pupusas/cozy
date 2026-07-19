//! `statusbar::TextRenderer`: glyph rasterization + atlas insertion +
//! `measure`/`draw_text` -- the bar's actual per-frame hot path (it repaints
//! on every module change, at worst every ~1s from the clock alone).
//!
//! Two regimes matter:
//!   * **cold** -- every glyph is a fresh atlas miss (rasterize + insert).
//!     This only happens once per distinct codepoint ever drawn, but a cold
//!     bar (first paint after startup) pays it for the whole initial label
//!     set at once.
//!   * **warm** -- every glyph is an atlas hit (`HashMap` lookup + blit).
//!     This is the steady-state cost paid every single repaint, since the
//!     clock/date/battery/etc. text is drawn fresh into the backbuffer each
//!     time (there's no dirty-rect tracking at the bar's text layer, unlike
//!     `grid`'s per-cell dirty tracking) -- so this is the number that
//!     actually matters for "does scrolling/refreshing the bar feel smooth".
//!
//! Falls back to the embedded bitmap font (no fixture dependency) if no
//! system TTF is found at any of `FONT_CANDIDATES`, so this still runs
//! (measuring the bitmap path instead) on a box without those fonts
//! installed -- see `pick_font`.
//!
//! Run:
//!   cargo bench -p statusbar-benches --bench text

use divan::counter::ItemsCount;
use platform::draw::{Canvas, Color};
use statusbar::TextRenderer;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Common desktop-Linux TTF locations, tried in order -- same fallback
/// posture as `crates/font/src/tests.rs`'s own fixture search, just wider
/// since this bench crate shouldn't hard-fail on a box that only has one of
/// these installed.
const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
];

fn pick_font() -> Option<&'static str> {
    FONT_CANDIDATES
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .copied()
}

const SAMPLE_TEXT: &str = "12:34:56  2024-01-01  100% \u{f0079}  50%/64%GiB \u{f4bc}";

#[divan::bench]
fn measure_warm(bencher: divan::Bencher) {
    let mut renderer = TextRenderer::load(pick_font(), 16.0);
    // Warm the atlas once, outside the timed region.
    renderer.measure(SAMPLE_TEXT);

    bencher
        .counter(ItemsCount::new(SAMPLE_TEXT.chars().count()))
        .bench_local(|| renderer.measure(divan::black_box(SAMPLE_TEXT)));
}

#[divan::bench]
fn draw_text_cold(bencher: divan::Bencher) {
    let mut backbuffer = vec![0u32; 400 * 32];
    bencher
        .counter(ItemsCount::new(SAMPLE_TEXT.chars().count()))
        .with_inputs(|| TextRenderer::load(pick_font(), 16.0))
        .bench_local_refs(|renderer| {
            let mut canvas = Canvas::new(&mut backbuffer, 400, 32);
            renderer.draw_text(
                &mut canvas,
                divan::black_box(SAMPLE_TEXT),
                0,
                0,
                Color::rgb(0xff, 0xff, 0xff),
                Color::rgb(0, 0, 0),
            );
        });
}

#[divan::bench]
fn draw_text_warm(bencher: divan::Bencher) {
    let mut renderer = TextRenderer::load(pick_font(), 16.0);
    let mut backbuffer = vec![0u32; 400 * 32];
    // Warm the atlas once, outside the timed region.
    {
        let mut canvas = Canvas::new(&mut backbuffer, 400, 32);
        renderer.draw_text(
            &mut canvas,
            SAMPLE_TEXT,
            0,
            0,
            Color::rgb(0xff, 0xff, 0xff),
            Color::rgb(0, 0, 0),
        );
    }

    bencher
        .counter(ItemsCount::new(SAMPLE_TEXT.chars().count()))
        .bench_local(|| {
            let mut canvas = Canvas::new(&mut backbuffer, 400, 32);
            renderer.draw_text(
                &mut canvas,
                divan::black_box(SAMPLE_TEXT),
                0,
                0,
                Color::rgb(0xff, 0xff, 0xff),
                Color::rgb(0, 0, 0),
            );
        });
}
