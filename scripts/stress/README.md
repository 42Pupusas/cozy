# UI performance stress tests

This terminal renders via a software framebuffer over Wayland `wl_shm`
(no GPU — see `PLAN.md`). The pipeline is:

```
pty_thread → parser_thread (vt_parser + grid) → SPSC<DirtySnapshot> → ui_thread (TermWindow)
```

`TermWindow::maybe_present` (`crates/platform/src/window.rs`) is the only
place that actually flips a frame, and it's gated on the compositor handing
back a `wl_callback.done` for the *previous* frame — so "FPS" here means
**presented frames per second**, paced by the compositor, not just how fast
we can push pixels.

Use the debug FPS instrumentation (below) together with these scripts to see
how the pipeline behaves under different kinds of load.

## Debug FPS counter

Set `TERMINAL_DEBUG_FPS` before launching the terminal:

```sh
TERMINAL_DEBUG_FPS=1   terminal   # live "NN.N fps" readout, top-right corner
TERMINAL_DEBUG_FPS=log terminal   # prints "terminal: NN.N fps" to stderr ~1x/sec
```

Unset (default) costs one enum comparison per frame — no measurable overhead.

The figure is a rolling average over the last 120 presents
(`crates/platform/src/window.rs`, `FrameStats`), so it settles a moment after
load starts/stops rather than jittering frame to frame.

## Running a scenario

Each script below just needs a terminal window open with the debug FPS
overlay or log enabled:

```sh
TERMINAL_DEBUG_FPS=log cargo run --release --bin terminal
# in the window that opens, run one of:
sh scripts/stress/scroll_flood.sh
sh scripts/stress/dense_cells.sh
sh scripts/stress/truecolor_flood.sh
sh scripts/stress/wide_unicode.sh
sh scripts/stress/resize_storm.sh   # run from a *different* terminal, targeting this window
sh scripts/stress/alt_screen_thrash.sh
sh scripts/stress/paste_flood.sh    # paste a big buffer into the window

# (or chmod +x scripts/stress/*.sh once, then run them directly with ./)
```

## Scenarios

1. **`scroll_flood.sh`** — `yes`-style raw throughput. Stresses vt_parser
   line-feed/scroll handling and how fast `parser_thread` can produce
   `DirtySnapshot`s before the 16-slot dirty ring (`RING_DIRTY` in
   `bins/terminal/src/main.rs`) backs up.
2. **`dense_cells.sh`** — full-screen redraw every line (like a `find /` or
   `ls -R` on a big tree with color), stressing `blit_snapshot`/`blit_cell` in
   `crates/platform/src/render.rs`.
3. **`truecolor_flood.sh`** — 24-bit SGR color gradient, stressing per-cell
   color resolution (`Theme::effective`, `render.rs`'s palette/RGB path) and
   attribute-change churn (every cell has a distinct `Attrs`).
4. **`wide_unicode.sh`** — CJK / emoji heavy text, stressing `text_layout`,
   glyph-atlas growth, and the `Cell::wide`/`spacer` double-width path in
   `crates/grid`.
5. **`resize_storm.sh`** — rapid resize events (Wayland-aware: uses `niri msg`
   on niri, `hyprctl` on Hyprland, or `swaymsg` on Sway, falling back to
   `xdotool` on X11/XWayland; otherwise drag-resize by hand), stressing
   `TermWindow::resize`. The pool now
   grows in place and shrinks lazily instead of being rebuilt every frame, so
   this confirms a resize flood no longer thrashes the shm pool; the grid
   reflow in the parser thread still runs each step.
6. **`alt_screen_thrash.sh`** — rapid `smcup`/`rmcup` (alternate screen)
   toggling, exercising grid mode switches and full-screen dirty snapshots.
7. **`paste_flood.sh`** — generates a large text blob to clipboard-paste by
   hand (`Ctrl+Shift+V`), stressing the bracketed-paste input path (not
   rendering, but good for completeness — the round trip goes through the
   `input`/`InputMsg` ring in `bins/terminal/src/main.rs`).

## `vtebench` (Alacritty's benchmark tool)

For a more rigorous, industry-standard comparison against other terminals,
install [vtebench](https://github.com/alacritty/vtebench):

```sh
git clone https://github.com/alacritty/vtebench /tmp/vtebench
cd /tmp/vtebench
cargo build --release
```

Then, inside a running instance of this terminal:

```sh
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks scrolling
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks dense_cells
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks light_cells
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks unicode
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks scrolling_in_region
/tmp/vtebench/target/release/vtebench -b /tmp/vtebench/benchmarks alt-screen-random-write
```

vtebench measures wall-clock time to *consume* the payload (i.e. read-side
throughput), which is a good proxy for "does the VT parser + grid + renderer
choke on a burst of output" independent of the compositor's frame pacing.
Combine with `TERMINAL_DEBUG_FPS=log` in a second terminal watching stderr to
see whether presented FPS dips while a benchmark runs.
