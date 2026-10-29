//! Full pipeline: `Parser::feed` -> `Grid::apply`, per event.
//!
//! `Event` borrows from the parser's internal buffers (OSC text in
//! particular), so events can't be pre-parsed and stashed for a
//! grid-only benchmark -- the parser has to run inline. Instead we get the
//! grid's isolated cost *programmatically*: run `parser.rs`'s `feed` bench
//! and this one on the same corpus, then subtract. Both benches use the
//! same corpus generators and geometry for exactly that reason -- don't
//! change one without the other.
//!
//! `Grid::new` is constructed ONCE per benchmarked corpus, outside the
//! timed region, and reused across every iteration. A grid allocates its
//! scrollback (up to 10 MB) as output fills it, so reconstructing it per
//! iteration (as `with_inputs` would) buries the real `feed`+`apply` cost
//! under page-fault noise. Harmless for divan's own timing (`with_inputs`
//! cost isn't counted), but it makes a `perf stat`/`record` pass over this
//! binary measure mostly memset instead of the hot path. Reusing one grid
//! also better matches production, where a `Grid` lives for the process's
//! lifetime, not one per frame.
//!
//! Run:
//!   cargo bench -p tty-benches --bench pipeline
//!   cargo bench -p tty-benches --bench pipeline -- --timer tsc

use divan::counter::BytesCount;
use grid::Grid;
use tty_benches::{COLS, CORPORA, Corpus, ROWS};
use vt_parser::Parser;

#[global_allocator]
static ALLOC: divan::AllocProfiler = divan::AllocProfiler::system();

fn main() {
    divan::main();
}

#[divan::bench(args = CORPORA)]
fn feed_apply(bencher: divan::Bencher, corpus: &Corpus) {
    let bytes = (corpus.generate)(COLS, ROWS, corpus.default_n);
    let mut parser = Parser::new();
    let mut grid = Grid::new(COLS, ROWS);
    bencher
        .counter(BytesCount::of_slice(&bytes))
        .bench_local(|| {
            parser.feed(divan::black_box(&bytes), |ev| {
                grid.apply(&ev);
            });
        });
}
