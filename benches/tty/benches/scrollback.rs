//! Scroll-*back* present cycle: `scroll_viewport` -> `take_dirty` ->
//! `blit_snapshot`, the path that drops fps when the user wheels up into
//! history. Every other bench in this crate feeds *output* bytes; none of
//! them exercise the viewport *view* path, which is a completely separate
//! branch in `take_dirty` (the `PendingRepaint::Viewport` full-frame
//! snapshot) and a separate branch in `blit_snapshot` (the signed
//! pixel-shift). This bench is the missing coverage.
//!
//! It reproduces the interactive scenario precisely: fill a deep scrollback
//! once (up front, untimed), then in the timed region simulate a present
//! loop that wheels the viewport one step per frame and runs the full
//! grid->blit present each frame -- exactly what the compositor drives when
//! you hold the wheel scrolling up through history.
//!
//! Three separate benches isolate where the per-frame cost lives:
//!   - `snapshot_only`  -- just `scroll_viewport` + `take_dirty` (grid work:
//!     the full-row-set build + shift computation).
//!   - `blit_only`      -- just `blit_snapshot` on a pre-built scroll frame
//!     (renderer work: the pixel-shift + revealed-edge rasterization).
//!   - `present_cycle`  -- both together, one wheel step per iteration: the
//!     honest end-to-end per-frame cost.
//!
//! `Grid::new` eagerly zero-fills a ~38 MB scrollback buffer, so it's built
//! exactly once and reused; see `blit.rs`/`snapshot.rs` for the same
//! rationale (a per-iteration rebuild buries the hot path under memset noise
//! for a `perf` pass).
//!
//! Run:
//!   cargo bench -p tty-benches --bench scrollback
//!   perf record on: target/release/deps/scrollback-*  (via the perf tool)

// Geometry casts mirror the real renderer's; never near truncation range.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::cell::RefCell;

use divan::counter::ItemsCount;
use grid::{DirtySnapshot, Grid};
use platform::draw::Canvas;
use platform::{Renderer, Theme};
use tty_benches::{COLS, ROWS};
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// How many lines of scrollback to accumulate before scrolling back through
/// it. Deep enough that the viewport spends the whole bench inside history
/// (never clamping back at the live bottom), matching a real "wheel up
/// through a long log" session.
const SCROLLBACK_LINES: usize = 4000;

/// Build a grid with `SCROLLBACK_LINES` of plain numbered output already
/// scrolled off into history, drained to the live bottom (offset 0).
fn primed_grid() -> Grid {
    let mut grid = Grid::new(COLS, ROWS);
    let mut parser = Parser::new();
    let mut bytes = Vec::new();
    for i in 0..SCROLLBACK_LINES {
        bytes.extend_from_slice(
            format!("{i}: the quick brown fox jumps over the lazy dog 0123456789\r\n").as_bytes(),
        );
    }
    parser.feed(&bytes, |ev| grid.apply(&ev));
    // Drain the output-driven dirty state so we start clean at offset 0,
    // exactly as the live loop would be after the flood settled.
    let _ = grid.take_dirty();
    grid
}

/// Grid work only: one wheel step up + the full-frame snapshot build.
#[divan::bench]
fn snapshot_only(bencher: divan::Bencher) {
    let grid = RefCell::new(primed_grid());
    bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
        let mut g = grid.borrow_mut();
        // One line per frame, the smallest (worst-case-frequency) wheel
        // step. Reset to a mid-history offset when we near the top so we
        // never clamp (which would emit a no-op frame and skew the mean).
        if g.viewport_offset() >= SCROLLBACK_LINES - 8 {
            g.scroll_viewport(-((SCROLLBACK_LINES / 2) as i32));
        }
        g.scroll_viewport(1);
        divan::black_box(g.take_dirty());
    });
}

/// Renderer work only: blit a pre-built one-step scroll frame repeatedly.
/// The frame carries a `shift`, so this measures the pixel-shift +
/// revealed-edge-row rasterization path specifically.
#[divan::bench]
fn blit_only(bencher: divan::Bencher) {
    let mut grid = primed_grid();
    let mut renderer = Renderer::builtin(Theme::builtin());
    let cell_w = renderer.cell_w() as usize;
    let cell_h = renderer.cell_h() as usize;
    let bw = COLS as usize * cell_w;
    let bh = ROWS as usize * cell_h;
    let mut backbuffer = vec![0u32; bw * bh];

    // A single one-line scroll-back frame (carries shift = -1).
    grid.scroll_viewport(1);
    let snap = grid.take_dirty();

    bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
        let mut canvas = Canvas::new(&mut backbuffer, bw as i32, bh as i32);
        renderer.blit_snapshot(&mut canvas, divan::black_box(&snap));
    });
}

/// Honest end-to-end per-frame present cost: wheel one step, snapshot, blit.
#[divan::bench]
fn present_cycle(bencher: divan::Bencher) {
    let grid = RefCell::new(primed_grid());
    let mut renderer = Renderer::builtin(Theme::builtin());
    let cell_w = renderer.cell_w() as usize;
    let cell_h = renderer.cell_h() as usize;
    let bw = COLS as usize * cell_w;
    let bh = ROWS as usize * cell_h;
    let backbuffer = RefCell::new(vec![0u32; bw * bh]);

    bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
        let mut g = grid.borrow_mut();
        if g.viewport_offset() >= SCROLLBACK_LINES - 8 {
            g.scroll_viewport(-((SCROLLBACK_LINES / 2) as i32));
        }
        g.scroll_viewport(1);
        let snap: DirtySnapshot = g.take_dirty();
        let mut bb = backbuffer.borrow_mut();
        let mut canvas = Canvas::new(&mut bb, bw as i32, bh as i32);
        renderer.blit_snapshot(&mut canvas, divan::black_box(&snap));
    });
}
