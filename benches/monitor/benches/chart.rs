//! `monitor::chart_cache::ChartCache::paint`: the per-card chart surface that
//! the live `MONITOR_PROFILE=1` trace pinned at ~0.42ms of a ~0.54ms paint --
//! 78% of the frame, across four cards. This bench exists to replace that
//! live-trace guess with a measured cost *and* an allocation count, because
//! the suspected waste was a memory-bandwidth one (a full-surface
//! `copy_pixels` blit on frames where the chart did not change), and a
//! `divan` bench with `AllocProfiler` is the only way to see per-call
//! allocations and byte throughput without the compositor's scheduling jitter
//! mixed in.
//!
//! Run:
//!   cargo bench -p monitor-benches --bench chart
//!
//! The three regimes map directly to what `App::run` drives each frame:
//! * `rebuild`     -- first paint / resize: rasterize the whole history.
//! * `advance_one` -- a new chart knot scrolled in: shift + redraw one column.
//! * `idle_glide`  -- the common case: no new column, background intact, so
//!                    the paint should now be a no-op (the optimization under
//!                    test). If this still allocates or moves bytes, the skip
//!                    is not working.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::doc_overindented_list_items,
    clippy::suboptimal_flops
)]

use std::collections::VecDeque;

use divan::counter::BytesCount;
use monitor::chart_cache::ChartCache;
use platform::draw::{Canvas, Color, Rect};

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// One minute of history at 4 knots/sec, matching the binary's `HISTORY`.
const HISTORY: usize = 60 * 4;

/// A representative card graph area. Card widths land near a third of a
/// 1080p-ish window minus margins; the height is the graph band below the
/// hero/detail rows. These are the geometries the live trace was measured at.
const CARD_W: i32 = 360;
const CARD_H: i32 = 150;

/// Deterministic, non-degenerate history so the renderer walks a real curve
/// rather than a constant the CPU can shortcut.
fn make_history(n: usize) -> VecDeque<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 * 0.11;
            50.0 + 40.0 * (t.sin() * 0.6 + (t * 0.37).cos() * 0.4)
        })
        .collect()
}

/// A target canvas sized to hold the card at (0,0), reused across iterations
/// exactly like `App`'s persistent backbuffer.
fn make_target() -> Vec<u32> {
    vec![0u32; (CARD_W * CARD_H) as usize]
}

/// First paint / resize: the whole surface is rasterized. This is the
/// expensive path we accept paying rarely (startup, geometry change).
#[divan::bench]
fn rebuild(bencher: divan::Bencher) {
    let values = make_history(HISTORY);
    bencher
        .counter(BytesCount::new(
            (CARD_W * CARD_H) as usize * size_of::<u32>(),
        ))
        .bench_local(|| {
            // A fresh cache each iteration forces `needs_rebuild` every time.
            let mut cache = ChartCache::new(Color::rgb(0x12, 0x14, 0x18));
            let mut target = make_target();
            let mut canvas = Canvas::new(&mut target, CARD_W, CARD_H);
            cache.paint(
                &mut canvas,
                Rect::new(0, 0, CARD_W, CARD_H),
                divan::black_box(&values),
                100.0,
                Color::rgb(0x3a, 0xd0, 0x7a),
                &[],
                HISTORY,
                divan::black_box(HISTORY as u64),
                true,
            );
            divan::black_box(&target);
        });
}

/// Steady advance: one new knot scrolled in since last paint. Shifts the
/// cached surface left and redraws the exposed right-edge column(s), then
/// blits. This is the per-knot cost (~4/sec live).
#[divan::bench]
fn advance_one(bencher: divan::Bencher) {
    let values = make_history(HISTORY);
    // Prime a cache at sequence N, then bench painting at N+1 repeatedly. The
    // cache is reused so only the incremental advance path runs; each call
    // advances the sequence by one so a column is always exposed.
    let mut cache = ChartCache::new(Color::rgb(0x12, 0x14, 0x18));
    let mut target = make_target();
    {
        let mut canvas = Canvas::new(&mut target, CARD_W, CARD_H);
        cache.paint(
            &mut canvas,
            Rect::new(0, 0, CARD_W, CARD_H),
            &values,
            100.0,
            Color::rgb(0x3a, 0xd0, 0x7a),
            &[],
            HISTORY,
            HISTORY as u64,
            true,
        );
    }
    let mut sequence = HISTORY as u64;
    bencher
        .counter(BytesCount::new(
            (CARD_W * CARD_H) as usize * size_of::<u32>(),
        ))
        .bench_local(|| {
            sequence += 1;
            let mut canvas = Canvas::new(&mut target, CARD_W, CARD_H);
            cache.paint(
                &mut canvas,
                Rect::new(0, 0, CARD_W, CARD_H),
                divan::black_box(&values),
                100.0,
                Color::rgb(0x3a, 0xd0, 0x7a),
                &[],
                HISTORY,
                divan::black_box(sequence),
                false,
            );
        });
}

/// The common case: no new knot (same sequence) and the background was NOT
/// cleared, so the target still holds last frame's identical chart. After the
/// optimization this must be a true no-op -- zero bytes moved, zero
/// allocations. `AllocProfiler`'s report on this bench is the proof.
#[divan::bench]
fn idle_glide(bencher: divan::Bencher) {
    let values = make_history(HISTORY);
    let mut cache = ChartCache::new(Color::rgb(0x12, 0x14, 0x18));
    let mut target = make_target();
    let sequence = HISTORY as u64;
    {
        let mut canvas = Canvas::new(&mut target, CARD_W, CARD_H);
        cache.paint(
            &mut canvas,
            Rect::new(0, 0, CARD_W, CARD_H),
            &values,
            100.0,
            Color::rgb(0x3a, 0xd0, 0x7a),
            &[],
            HISTORY,
            sequence,
            true,
        );
    }
    bencher
        .counter(BytesCount::new(
            (CARD_W * CARD_H) as usize * size_of::<u32>(),
        ))
        .bench_local(|| {
            let mut canvas = Canvas::new(&mut target, CARD_W, CARD_H);
            cache.paint(
                &mut canvas,
                Rect::new(0, 0, CARD_W, CARD_H),
                divan::black_box(&values),
                100.0,
                Color::rgb(0x3a, 0xd0, 0x7a),
                &[],
                HISTORY,
                divan::black_box(sequence),
                false,
            );
        });
}
