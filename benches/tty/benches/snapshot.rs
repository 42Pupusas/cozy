//! `Grid::take_dirty` and last-writer-wins multi-snapshot coalescing.
//!
//! Models the structural fact `platform/examples/flood_bench.rs` documents:
//! between two compositor vsyncs the parser thread can publish several
//! `DirtySnapshot`s. Two things get timed separately here:
//!   - `take_dirty`: cost of draining the grid's dirty accumulator once.
//!   - `coalesce`: cost of merging several snapshots into one effective
//!     frame (last-writer-wins by row) before a present, ported straight
//!     from `flood_bench`'s `coalesce` helper.
//!
//! `Grid::new` eagerly allocates and zero-fills a `2 * (rows +
//! SCROLLBACK_MAX)`-row buffer (~38 MB at the default 10k-line scrollback),
//! so both benches build exactly one `Grid` up front and reuse it/its
//! output across iterations rather than reconstructing it per iteration --
//! otherwise a `perf stat`/`record` pass over this binary measures mostly
//! memset. `take_dirty` re-dirties the same persistent grid inside
//! `with_inputs` (excluded from divan's own timing) each iteration since
//! it's a draining operation; `coalesce` only reads its input so the
//! snapshot list is built exactly once.
//!
//! Run:
//!   cargo bench -p tty-benches --bench snapshot

use std::cell::RefCell;

use grid::{DirtyRow, DirtySnapshot, Grid};
use tty_benches::{COLS, CORPORA, Corpus, ROWS};
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

/// Snapshots produced per simulated vsync interval — same constant
/// `flood_bench` uses, representative of the parser's batch cap outrunning
/// the compositor's ~16ms vsync under a flood.
const SNAPS_PER_PRESENT: usize = 6;

#[divan::bench(args = CORPORA)]
fn take_dirty(bencher: divan::Bencher, corpus: &Corpus) {
    let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);
    let grid = RefCell::new(Grid::new(COLS, ROWS));
    let parser = RefCell::new(Parser::new());
    bencher
        .with_inputs(|| {
            // Re-dirty the persistent grid (excluded from timing) so each
            // iteration's take_dirty() has something real to drain.
            parser
                .borrow_mut()
                .feed(&bytes, |ev| grid.borrow_mut().apply(&ev));
        })
        .bench_local_refs(|()| divan::black_box(grid.borrow_mut().take_dirty()));
}

#[divan::bench(args = CORPORA)]
fn coalesce(bencher: divan::Bencher, corpus: &Corpus) {
    let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);
    // Built once: coalesce_last_writer_wins only reads its input, so the
    // same snapshot list is valid input for every iteration.
    let mut grid = Grid::new(COLS, ROWS);
    let mut parser = Parser::new();
    let mut snaps = Vec::with_capacity(SNAPS_PER_PRESENT);
    let chunk = bytes.len() / SNAPS_PER_PRESENT.max(1);
    for part in bytes.chunks(chunk.max(1)).take(SNAPS_PER_PRESENT) {
        parser.feed(part, |ev| grid.apply(&ev));
        let snap = grid.take_dirty();
        if !snap.is_empty() {
            snaps.push(snap);
        }
    }
    bencher.bench_local(|| coalesce_last_writer_wins(divan::black_box(&snaps)));
}

/// Ported verbatim from `platform/examples/flood_bench.rs::coalesce`.
fn coalesce_last_writer_wins(snaps: &[DirtySnapshot]) -> DirtySnapshot {
    let mut by_row: std::collections::BTreeMap<u16, DirtyRow> = std::collections::BTreeMap::new();
    for snap in snaps {
        for row in &snap.rows {
            by_row.insert(row.row, row.clone());
        }
    }
    DirtySnapshot {
        rows: by_row.into_values().collect(),
        cols: COLS,
        cursor: grid::CursorInfo::default(),
        cursor_changed: false,
        content_delta: 0,
        kind: grid::SnapshotKind::Incremental { scroll_span: None },
    }
}
