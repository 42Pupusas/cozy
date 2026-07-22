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
- `examples/ascii_512mb.rs` — one-shot, production-shaped measurement of
  feeding 512 MiB of ASCII through 4 KiB PTY chunks, parser/grid batching, and
  the real renderer/backbuffer present path.

## Measuring a 512 MiB ASCII flood

Run the workload (release mode is important):

    cargo run --release -p tty-benches --example ascii_512mb

The default uses the embedded bitmap renderer. To exercise the real TTF glyph
path, pass the same font file used by the terminal:

    cargo run --release -p tty-benches --example ascii_512mb -- font=/path/to/font.ttf

Use `bytes=...` for a short smoke test. The output reports elapsed wall time,
MiB/s, the number of 4 KiB chunks, snapshots, and presents. The timed region
includes VT parsing, grid application, dirty snapshot construction, and
`Renderer::blit_snapshot` into a real backbuffer. It excludes the Wayland
compositor, compositor queueing, and OS PTY scheduling; those cannot be made
reproducible by a headless benchmark. Run it several times and use the median;
the first run warms code/data pages.

This is deliberately an example rather than a repeated Divan bench: 512 MiB
is a sustained workload, and repeatedly rebuilding it would measure allocator
and page-cache effects more than terminal throughput.

`CORPORA` also includes `control_heavy`, which has no `scripts/stress/*.sh`
analogue: it cycles mouse-tracking mode changes, DECCKM/DECAWM/DECOM/focus-
events, the Kitty keyboard flag stack, DECSCUSR, tab stops (HTS/TBC/CHT/CBT),
DEC Special Graphics (SO/SI) line drawing, combining accents, BEL, and a
DECRQSS round trip -- the mode-negotiation/DCS surface the other corpora
(all Print/SGR/scroll-heavy) never touch.

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

Add a `fn(cols: u16, rows: u16, n: usize) -> Vec<u8>` associated function on
`Corpora` in `src/lib.rs` and list it in `CORPORA`; every
`#[divan::bench(args = CORPORA)]` benchmark picks it up automatically.
