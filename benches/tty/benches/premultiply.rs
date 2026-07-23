//! `platform::premultiply::Premultiplier`: the custom-painter ARGB rebuild
//! that `TermWindow::update_custom_argb` runs on every present of a
//! translucent custom-painted window (e.g. `monitor` under compositor
//! opacity < 1.0).
//!
//! This exists because live GUI trace logs (`TERMINAL_DEBUG_FPS=log`) are
//! not a reliable way to validate a hot-loop change: they mix in compositor
//! scheduling jitter, shm submit latency, and whatever else the desktop is
//! doing, and re-running the interactive app by hand between edits is slow
//! and easy to eyeball wrong. A `divan` bench isolates exactly the kernel
//! under test, with `AllocProfiler` reporting whether any path is
//! secretly allocating per call (a very plausible alternate explanation for
//! "18ms/frame that a small vector loop shouldn't cost").
//!
//! Run:
//!   cargo bench -p tty-benches --bench premultiply
//!
//! `-p tty-benches --bench premultiply -- --filter alloc` isolates the
//! allocation-counting variant if the timing numbers alone leave any doubt.

// Bench geometry/byte-count math never comes close to the range where
// these casts could truncate or wrap -- see `blit.rs`'s identical rationale
// for its own allow-list.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use divan::counter::BytesCount;
use platform::premultiply::Premultiplier;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Window pixel-buffer sizes to bench across: a small terminal window, the
/// `tty-benches` "big" geometry converted to pixels at a typical cell size,
/// and a plausible large monitor (3440x1440 ultrawide) -- the size class
/// where the live 18-30ms/frame regression was actually observed.
const PIXEL_COUNTS: &[usize] = &[
    80 * 24 * 8 * 16,  // COLS x ROWS at an 8x16 cell -> small terminal window
    240 * 70 * 8 * 16, // BIG_COLS x BIG_ROWS at an 8x16 cell
    3440 * 1440,       // ultrawide monitor, the reported live regression size
];

/// Deterministic non-constant pixel data -- constant input risks the
/// compiler/CPU taking a degenerate fast path (e.g. branch prediction on an
/// all-zero buffer) that a real dashboard's varied pixels never get.
fn make_pixels(n: usize) -> Vec<u32> {
    (0..n)
        .map(|i| 0x00_010203u32.wrapping_mul(i as u32 + 1) & 0x00ff_ffff)
        .collect()
}

/// Steady-state cost: buffers allocated once, outside the timed region,
/// matching how `TermWindow` reuses `backbuffer`/`custom_argb` across
/// presents rather than allocating fresh each frame.
#[divan::bench(args = PIXEL_COUNTS)]
#[allow(clippy::trivially_copy_pass_by_ref)] // divan's `args` macro requires this by-ref pattern.
fn run(bencher: divan::Bencher, &n: &usize) {
    let pre = Premultiplier::new();
    let src = make_pixels(n);
    let mut dst = vec![0u32; n];
    bencher
        .counter(BytesCount::new(n * std::mem::size_of::<u32>()))
        .bench_local(|| {
            pre.run(divan::black_box(180), divan::black_box(&src), &mut dst);
        });
}

/// Same kernel, but allocates `dst` fresh every iteration. If this is
/// dramatically slower than `run` above, per-frame allocation (not the
/// arithmetic) is the real cost -- `AllocProfiler`'s report on this bench
/// will show the allocation count/bytes directly either way.
#[divan::bench(args = PIXEL_COUNTS)]
#[allow(clippy::trivially_copy_pass_by_ref)] // divan's `args` macro requires this by-ref pattern.
fn run_with_fresh_allocation(bencher: divan::Bencher, &n: &usize) {
    let pre = Premultiplier::new();
    let src = make_pixels(n);
    bencher
        .counter(BytesCount::new(n * std::mem::size_of::<u32>()))
        .bench_local(|| {
            let mut dst = vec![0u32; n];
            pre.run(divan::black_box(180), divan::black_box(&src), &mut dst);
            divan::black_box(&dst);
        });
}

/// The exact scalar `channel * alpha / 255` loop this kernel replaced
/// (`TermWindow::premultiply_pixel`'s formula), as a baseline so the SIMD
/// speedup is a real measured ratio, not a guess.
#[divan::bench(args = PIXEL_COUNTS)]
#[allow(clippy::trivially_copy_pass_by_ref)] // divan's `args` macro requires this by-ref pattern.
fn scalar_baseline(bencher: divan::Bencher, &n: &usize) {
    let src = make_pixels(n);
    let mut dst = vec![0u32; n];
    bencher
        .counter(BytesCount::new(n * std::mem::size_of::<u32>()))
        .bench_local(|| {
            let alpha = divan::black_box(180u32);
            for (source, dest) in src.iter().zip(dst.iter_mut()) {
                let rgb = *source & 0x00ff_ffff;
                let r = ((rgb >> 16) & 0xff) * alpha / 255;
                let g = ((rgb >> 8) & 0xff) * alpha / 255;
                let b = (rgb & 0xff) * alpha / 255;
                *dest = (alpha << 24) | (r << 16) | (g << 8) | b;
            }
        });
}
