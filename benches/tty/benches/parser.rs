//! `vt_parser` alone: `Parser::feed` with a no-op callback.
//!
//! This isolates the state-machine cost from grid mutation. `vt_parser`'s
//! own docs claim the hot path is entirely allocation-free (all allocation
//! happens once at startup, for the SPSC ring) — the `AllocProfiler` below
//! turns that claim into a tripwire: any nonzero alloc count printed here
//! for the steady-state `feed` loop is a regression, not a design choice.
//!
//! Run:
//!   cargo bench -p tty-benches --bench parser
//!   cargo bench -p tty-benches --bench parser -- --timer tsc   # cycles, not wall time

use divan::counter::BytesCount;
use tty_benches::{COLS, CORPORA, Corpus, ROWS};
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

#[divan::bench(args = CORPORA)]
fn feed(bencher: divan::Bencher, corpus: &Corpus) {
    let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);
    bencher
        .counter(BytesCount::of_slice(&bytes))
        .with_inputs(Parser::new)
        .bench_local_refs(|parser| {
            parser.feed(divan::black_box(&bytes), |ev| {
                divan::black_box(&ev);
            });
        });
}
