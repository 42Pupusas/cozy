# statusbar-benches

`divan` micro-benchmarks for the `statusbar` crate's hot spots, mirroring
`benches/tty`'s structure and conventions (same `divan::AllocProfiler`
allocation-tripwire posture, same `cargo bench -p <crate> --bench <file>`
invocation).

## Layout

- `benches/text.rs` — `TextRenderer::measure`/`draw_text`: glyph
  rasterization + atlas insertion (cold) vs. atlas hit + blit (warm). The
  warm path is the one that matters: the bar repaints its full text on
  every module change (no dirty-rect tracking at the text layer, unlike
  `grid`'s per-cell dirty tracking), so this runs at least once a second
  from the clock module alone.
- `benches/scroll.rs` — `ScrollAccumulator`: the wheel-scroll debounce math
  that runs on every `wl_pointer.axis` event while hovering a scrollable
  segment. Cheap by construction; the interesting signal is the alloc
  tripwire (must be zero, like `vt_parser`/`grid`'s hot paths in
  `benches/tty`), not the wall-time.
- `benches/colors.rs` — `module_color`: per-module color lookup, called
  once per segment on every repaint (up to ~11 times/frame in `paint`).

## Why these three

Everything else in `statusbar` is either I/O-bound (the `modules::*`
sensor readers all shell out to `/proc`, `/sys`, or a CLI — dominated by
syscall/process-spawn latency, not CPU, so a divan micro-bench would
mostly measure noise) or a thin Wayland wire-protocol wrapper
(`bar_window.rs` — already covered by `platform`'s own connection-level
concerns, and needs a live compositor to exercise meaningfully). `text`,
`scroll`, and `colors` are the parts of this crate that are pure
CPU-bound logic runnable headless, and not coincidentally are also the
parts that have already shipped real bugs in this bar's short life
(mis-encoded codepoints, a scroll-accumulation regression that took three
iterations to get right, a battery-color threshold that silently never
fired) — exactly the kind of logic worth pinning down with both a
correctness test (`crates/statusbar/src/{colors,scroll,modules}.rs`'s own
`#[cfg(test)]` modules) *and* a performance baseline.

## Running

    cargo bench -p statusbar-benches                  # everything
    cargo bench -p statusbar-benches --bench text      # one file

## Allocation tripwire

Every bench file sets `divan::AllocProfiler` as `#[global_allocator]`.
`scroll.rs` and the warm-path benches in `text.rs`/`colors.rs` are
expected to print **no alloc block at all** — a regression there (e.g. a
future change that makes `module_color` build a `String`, or
`ScrollAccumulator` grow a `Vec`) is worth chasing down. `draw_text_cold`
in `text.rs` is expected to allocate (fresh atlas insertions grow
backing storage) — its alloc count is a magnitude signal, not a
zero-tolerance tripwire.

## Font fixture

`text.rs` searches a short list of common desktop-Linux TTF paths
(`FONT_CANDIDATES`) and falls back to `TextRenderer`'s embedded bitmap
font (measuring that path instead) if none are found, so this still runs
headless/in CI without a hard dependency on a specific font package being
installed — unlike `crates/font`'s own tests, which hard-fail without
`fonts-jetbrains-mono` since they're testing the TTF parser itself, not a
downstream consumer that's designed to degrade gracefully.
