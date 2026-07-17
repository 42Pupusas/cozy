//! `Renderer::blit_snapshot`: the present-path blit loop, divan-ified from
//! `platform/examples/flood_bench.rs`.
//!
//! As in `flood_bench`, snapshot building (parser + grid work) happens
//! entirely up front, outside the timed region -- only the blit into the
//! backbuffer is measured, matching `TermWindow::maybe_present`'s actual
//! hot loop. Unlike an earlier version of this file, the snapshots and
//! backbuffer are built exactly ONCE (not per-iteration via `with_inputs`):
//! `Grid::new` eagerly zero-fills a ~38 MB scrollback buffer, so
//! reconstructing it every iteration buried the blit cost under page-fault
//! noise under `perf stat`/`record` (divan's own numbers were unaffected,
//! since `with_inputs` cost isn't counted -- but a whole-process profiler
//! doesn't know that). `blit_snapshot` only reads the snapshot and
//! overwrites the backbuffer, so reusing both across iterations is valid.
//!
//! Run:
//!   cargo bench -p tty-benches --bench blit

// Buffer geometry (COLS/ROWS x a font's small cell_w/cell_h) never comes
// close to i32/usize truncation or sign-loss range -- these casts mirror the
// real renderer's own (also-allowed) geometry casts, not a real risk.
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use divan::counter::ItemsCount;
use grid::{DirtySnapshot, Grid};
use platform::draw::Canvas;
use platform::{Renderer, Theme};
use tty_benches::{COLS, CORPORA, Corpus, ROWS};
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Snapshots per simulated present — same rationale as `snapshot.rs` /
/// `flood_bench`.
const SNAPS_PER_PRESENT: usize = 6;

#[divan::bench(args = CORPORA)]
fn blit_snapshot(bencher: divan::Bencher, corpus: &Corpus) {
    let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);

    let mut renderer = Renderer::builtin(Theme::builtin());
    let cell_w = renderer.cell_w() as usize;
    let cell_h = renderer.cell_h() as usize;
    let bw = COLS as usize * cell_w;
    let bh = ROWS as usize * cell_h;

    let mut grid = Grid::new(COLS, ROWS);
    let mut parser = Parser::new();
    let mut snaps: Vec<DirtySnapshot> = Vec::with_capacity(SNAPS_PER_PRESENT);
    let chunk = bytes.len() / SNAPS_PER_PRESENT.max(1);
    for part in bytes.chunks(chunk.max(1)).take(SNAPS_PER_PRESENT) {
        parser.feed(part, |ev| grid.apply(&ev));
        let snap = grid.take_dirty();
        if !snap.is_empty() {
            snaps.push(snap);
        }
    }

    let mut backbuffer = vec![0u32; bw * bh];

    bencher
        .counter(ItemsCount::new(SNAPS_PER_PRESENT))
        .bench_local(|| {
            let mut canvas = Canvas::new(&mut backbuffer, bw as i32, bh as i32);
            for snap in &snaps {
                renderer.blit_snapshot(&mut canvas, divan::black_box(snap));
            }
        });
}
