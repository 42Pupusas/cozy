# tty-benches

`divan` micro-benchmarks for the tty pipeline: `pty -> vt_parser -> grid ->
platform` (renderer), plus `text_layout` and `input` on the side.

## Layout

- `src/lib.rs` — deterministic VT-stream corpora (`CORPORA`), one generator
  per `scripts/stress/*.sh` interactive stress script, so a regression here
  has a direct "go run this script and watch it happen live" analogue.
- `benches/parser.rs` — `vt_parser::Parser::feed` alone (no-op callback).
- `benches/pipeline.rs` — `feed` -> `Grid::apply`, the full hot path.
- `benches/snapshot.rs` — `Grid::take_dirty` and last-writer-wins multi-
  snapshot coalescing (ported from `flood_bench`).
- `benches/blit.rs` — `Renderer::blit_snapshot`, the present-path blit loop
  (divan-ified `platform/examples/flood_bench.rs`; that example stays for
  ad-hoc interactive `perf record` sessions).
- `benches/layout.rs` — `text_layout::Layouter::layout` over ascii/CJK/BiDi
  fixtures.
- `benches/input.rs` — `input::Encoder::encode_key` across all three
  keyboard modes.

## Why parser and pipeline are separate benches

`vt_parser::TermEvent` borrows from the parser's internal buffers (OSC text in
particular), so events can't be parsed once and replayed into the grid —
the parser has to run inline with every grid apply. There is therefore no
"grid alone" bench. Instead:

    grid cost ≈ pipeline(corpus) − parser(corpus)

Both benches use the same corpus generators and geometry so that
subtraction is meaningful — don't change one without the other.

## Running

    cargo bench -p tty-benches                      # everything
    cargo bench -p tty-benches --bench parser        # one file
    cargo bench -p tty-benches --bench parser -- --timer tsc   # cycles, not wall time

## Allocation tripwire

Every bench file sets `divan::AllocProfiler` as `#[global_allocator]`, so
alloc/dealloc counts and bytes print alongside timings automatically.
`vt_parser` and `grid::Grid::apply` are documented as allocation-free on
their hot path (all allocation happens once at startup) — as of this
writing `parser.rs` and `pipeline.rs` print **no alloc block at all**,
i.e. zero allocations. If a future change makes one appear, that's a
regression to chase down, not a new baseline to accept.

`layout.rs`, `snapshot.rs`'s `coalesce`, and `blit.rs` are expected to
allocate (`Vec`/`String`/`BTreeMap` construction) — their alloc counts are
useful as a *magnitude* signal (did this change 2x the allocation traffic?)
rather than a zero-tolerance tripwire.

Note: alloc profiling adds a little timing overhead. For a clean cycle
count, prefer a `--timer tsc` run without needing to think about allocs
separately — the two concerns are read from the same output, just be aware
mixing conclusions from a single run means the timing includes the
profiler's bookkeeping.

## Geometry

`COLS`/`ROWS` = 80×24 (matches the stress scripts' `COLUMNS:-80` default).
`BIG_COLS`/`BIG_ROWS` = 240×70 also exported from `src/lib.rs` for future
benches that want to catch effects that only show up at larger geometry.

## Adding a corpus

Add a `fn(cols: u16, rows: u16, n: usize) -> Vec<u8>` to `src/lib.rs` and
list it in `CORPORA`; every `#[divan::bench(args = CORPORA)]` benchmark
picks it up automatically.
