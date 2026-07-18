# terminal — Project Plan

A from-scratch terminal emulator written in pure Rust with minimal external
dependencies. Think Alacritty / Ghostty but owned end-to-end.

---

## Guiding principles

- **Zero external deps** where possible. Every crate we pull in must be
  justified. Prefer rolling our own tooling crates.
- **Rust only.** No C glue, no bindgen, no `build.rs` shelling out to system
  libraries.
- **No async runtimes.** No Tokio, no async-std, no `Future`s in the hot path.
  Concurrency is expressed as threads communicating over ring buffers.
- **Message-passing over shared state.** All cross-thread and cross-boundary
  communication goes through ring buffers. Two flavours:
  - *Kernel boundary* → `io_uring` via `ququmatz` / `quetzal-uring`
  - *Intra-process* → lock-free userspace rings via `quetzalcoatl`
- **Workspace-first.** Each concern is its own crate under `crates/`. The top-
  level binary in `bins/terminal` wires them together.
- **Full clippy lints.** `pedantic`, `nursery`, `perf`, `style` — doc-related
  lints are the only exceptions allowed.

---

## Concurrency model

We avoid async/await entirely. Instead every component is a thread (or a small
fixed pool of threads) that loops over its input ring, does work, and writes
results to its output ring. All blocking happens either in the kernel (via
`io_uring_enter`) or by parking on a ring buffer's futex-style wait path — never
in a runtime scheduler.

### Tooling crates

| Crate | Purpose | Where used |
|-------|---------|-----------|
| `ququmatz` | Zero-dep `io_uring` bindings (raw syscalls, no libc, `#![no_std]`). Owns the SQ/CQ ring mmap, exposes `Sqe` builders for all ops. Also owns `Inotify` and `EventFd` wrappers. | `crates/pty`, `crates/platform`, `crates/config` |
| `quetzal-uring` | Lock-free MPSC submission + SPMC completion *directly on the kernel SQ/CQ rings* — no intermediate userspace copy. Three drive models: `drain_wait` (single consumer, blocks in kernel), `demux::Router` (N shards, each with their own completion inbox), external pumper. | `crates/platform` (event loop pumper), `crates/pty` (PTY read/write ops) |
| `quetzalcoatl` | Lock-free userspace ring buffers — SPSC / MPSC / SPMC / MPMC / broadcast. All topologies, zero deps, blocking & non-blocking variants, zero-copy typestate API (`reserve → write → commit`), batch `drain`. | everywhere intra-process |

### Thread topology

```
┌─────────────────────────────────────────────────────────────────────────┐
│  KERNEL BOUNDARY  (io_uring / ququmatz + quetzal-uring)                 │
│                                                                         │
│   PTY master fd  ──read SQE──►  io_uring  ──CQE──►  pty-thread         │
│   PTY master fd  ◄─write SQE─  io_uring  ◄─SQE──   input-thread        │
│   inotify fd     ──read SQE──►  io_uring  ──CQE──►  config-thread       │
│   Wayland fd     ──read SQE──►  io_uring  ──CQE──►  platform-thread     │
│   Wayland fd     ◄─write SQE─  io_uring  ◄─SQE──   renderer-thread     │
└─────────────────────────────────────────────────────────────────────────┘
         │                                         ▲
         │  INTRA-PROCESS  (quetzalcoatl rings)    │
         ▼                                         │
  pty-thread                               renderer-thread
  (SPSC producer)──► raw-bytes ring ──►  vt-parser-thread
                                         (SPSC producer)──► event ring ──► grid-thread
                                                                            (SPSC producer)──► dirty-region ring ──► renderer-thread

  input-thread ◄── input-cmd ring ◄── (SPSC producer) platform-thread
  config-thread ──► config-event ring ──► (broadcast) all threads
```

### Ring buffer selection guide

| Situation | Topology | Crate |
|-----------|----------|-------|
| PTY master fd reads/writes | fd ops → kernel via SQE | `ququmatz` SQE + `quetzal-uring` |
| Raw bytes PTY → VT parser | one producer, one consumer | `quetzalcoatl::spsc` |
| VT events → grid | one producer, one consumer | `quetzalcoatl::spsc` |
| Dirty regions → renderer | one producer, one consumer | `quetzalcoatl::spsc` |
| User input → input encoder | platform fan-in | `quetzalcoatl::mpsc` |
| Encoded bytes → PTY write SQE | one producer, one consumer | `quetzalcoatl::spsc` |
| Config changes → all threads | one publisher, N subscribers | `quetzalcoatl::broadcast` |
| Wayland protocol frames | single renderer → kernel | `quetzal-uring` SPSC write path |

---

## Why a PTY instead of `std::Command`

`std::Command` gives a child process three byte pipes (`stdin`/`stdout`/
`stderr`). That breaks interactive programs in several ways:

| Problem | Cause |
|---------|-------|
| Shell disables prompts & color | `isatty(stdin)` returns `false` |
| No window size | No `TIOCGWINSZ` ioctl available |
| `Ctrl+C` doesn't send `SIGINT` | Signal dispatch goes through the TTY line discipline, not a pipe |
| No line editing / echo / erase | Kernel TTY layer is absent |

A **pseudoterminal (PTY)** is a kernel-level fiction: a master/slave fd pair
where the kernel pretends both ends are a real serial terminal. The shell sees
`isatty(0) == true`, we can `ioctl(master, TIOCSWINSZ, …)` to push resize
events, and the line discipline handles `Ctrl+C` → `SIGINT` transparently.

Implementation: `posix_openpt` / `forkpty` syscalls via raw `unsafe` libc FFI.
The master fd is then handed to `io_uring` (via `ququmatz`) for all read/write
operations — no blocking `read(2)` / `write(2)` calls on the hot path.

---

## Crate map

```
terminal/
├── Cargo.toml                  ← workspace root, lint config
├── PLAN.md                     ← this file
├── bins/
│   └── terminal/               ← entry point; wires all crates together
├── crates/
│   ├── pty/                    ← pseudoterminal: forkpty, resize, io_uring I/O
│   ├── vt_parser/              ← VT/ANSI escape sequence state machine
│   ├── grid/                   ← 2-D cell grid, scrollback, alternate screen
│   ├── text_layout/            ← text measurement + line-breaking (pretext port)
│   ├── font/                   ← TTF/OTF loader, glyph rasteriser, atlas
│   ├── renderer/               ← GPU/software draw: grid → pixels
│   ├── platform/               ← window + event loop (built on paint scaffold)
│   ├── input/                  ← keyboard/mouse → terminal byte sequences
│   ├── config/                 ← TOML config (font, colors, shell, keybinds)
│   └── tlog/                   ← compile-time-gated, runtime-selectable tracing
└── benches/
    └── …                       ← criterion suites per hot-path crate
```

---

## Crate responsibilities

### `crates/pty`

Safe wrapper around raw POSIX pseudoterminal syscalls, with all I/O driven
through `io_uring`.

- `posix_openpt` → open a PTY master fd
- `forkpty` → spawn a child with the slave fd as its controlling terminal
- `TIOCSWINSZ` ioctl → push window-size changes to the running process
- `TIOCGWINSZ` ioctl → query current window size
- Process lifecycle: `waitpid`, `SIGCHLD` handling via `signalfd` + io_uring
  read SQE, exit-code propagation
- **I/O model:** master fd registered with a `quetzal-uring` `UringQueue`.
  - Reads: `Sqe::read(master_fd, …)` submitted; CQEs land in the pty-thread's
    `CqConsumer`; raw byte chunks pushed onto an SPSC `quetzalcoatl` ring
    towards the VT parser thread.
  - Writes: input-encoded bytes arrive on an SPSC ring from `crates/input`;
    pty-thread submits `Sqe::write(master_fd, …)` SQEs.
- No blocking `read`/`write` syscalls. No async runtime.

**Tooling deps:** `ququmatz` (SQE builders, raw fd ops), `quetzal-uring`
(lock-free SQ/CQ wrappers), `quetzalcoatl` (SPSC byte-chunk rings).

---

### `crates/vt_parser`

State machine that consumes raw bytes from the PTY master fd and emits typed
terminal events.

Sequences handled:

- **C0** control characters (`BEL`, `BS`, `HT`, `LF`, `CR`, `SO`, `SI`, …)
- **ESC** sequences (charset designation, index, reverse index, …)
- **CSI** sequences (cursor movement, SGR attributes, erase, scroll, …)
- **OSC** sequences (window title, color palette, hyperlinks, …)
- **DCS** sequences (DECDLD, Sixel, TMUX passthrough, …)
- **APC** / **PM** / **SOS** (ignored but consumed cleanly)

**Threading:** runs on its own thread, consuming from an SPSC
`quetzalcoatl::spsc` ring (raw bytes from pty-thread), producing parsed
`TermEvent` values onto a second SPSC ring towards the grid-thread.
No allocations in the hot path — the state machine is a `u8`-indexed table.

**Deps:** none. Pure state machine.

---

### `crates/grid`

The terminal's screen model.

- 2-D array of `Cell { char, fg, bg, attrs }` (bold, italic, underline,
  blink, inverse, …)
- Primary screen + alternate screen (switched by `smcup`/`rmcup`)
- Scrollback buffer with configurable max lines
- Cursor state: position, visibility, shape, saved cursor (`DECSC`/`DECRC`)
- `DirtyRegion` tracker so the renderer only redraws changed cells
- Selection model (range of cells → clipboard text)

**Threading:** grid-thread owns the `Grid` exclusively — no shared-memory
access. It consumes `TermEvent`s from the VT parser SPSC ring, mutates the
grid, and pushes `DirtyRegion` snapshots onto an SPSC ring towards the
renderer-thread.

**Deps:** none.

---

### `crates/text_layout`

Port of [chenglou/pretext](https://github.com/chenglou/pretext) to pure Rust.
Used for GUI chrome (title bars, config panels, overlays) — **not** the
terminal grid, which uses fixed-width cell metrics.

#### What pretext is

A text **measurement and line-breaking engine**. Given a string, a font
descriptor, and a max width, it answers: *where do the line breaks go, and how
tall is the resulting block?*

#### Module map (mirroring pretext's src/)

| Rust module | TS original | Notes |
|-------------|-------------|-------|
| `analysis` | `analysis.ts` | Whitespace normalisation, word/grapheme segmentation, break-kind classification. Swap `Intl.Segmenter` for our own UAX #29 tables. |
| `measurement` | `measurement.ts` | Segment advance widths. Swap `Canvas.measureText()` for UAX #11 East-Asian-Width + TTF `hmtx` table reads (see `crates/font`). |
| `line_break` | `line-break.ts` | **Direct 1:1 port.** Pure arithmetic over cached widths; no JS-isms. |
| `layout` | `layout.ts` | Public API: `prepare`, `layout`, `layout_with_lines`, cursor types. Direct port. |
| `bidi` | `bidi.ts` | Simplified Unicode BiDi metadata for RTL-aware rendering. Direct port of range tables + state machine. |

#### Why the terminal shortcut works

In the terminal grid itself every glyph is **exactly 1 or 2 cells wide**
(UAX #11 East Asian Width). The cell size is fixed by the font's advance width
for a reference glyph (e.g. `M`). No per-glyph font metrics or complex shaping
needed for grid rendering.

`text_layout` is only needed for GUI chrome where proportional fonts and
variable-width text appear.

**Deps:** none (UAX tables are generated and checked in as Rust arrays).

---

### `crates/font`

TTF/OTF font loading, glyph rasterisation, and atlas management.

- Parse font files directly (no FreeType, no `font-kit`)
  - `cmap` table → Unicode → glyph ID
  - `hmtx` table → advance widths (feeds `crates/text_layout` measurement)
  - `glyf` / `CFF` table → glyph outlines
  - `head` / `hhea` / `OS/2` → metrics (ascender, descender, line gap)
- Scan-line rasteriser for glyph outlines → 8-bit alpha mask
- **Glyph atlas:** pack rasterised glyphs into a texture; expose dirty regions
  for GPU upload
- Cache: `HashMap<(GlyphId, PixelSize), AtlasSlot>`
- Font fallback chain: primary font → emoji font → system fallback

**Deps:** none. TTF/OTF parsing is table reads + arithmetic.

---

### `crates/renderer`

Draws the terminal grid (and GUI chrome) to the screen.

- Reads `Grid` + `DirtyRegion` snapshots from an SPSC `quetzalcoatl` ring
  (grid-thread → renderer-thread)
- Reads glyph atlas from `crates/font`
- Two backends (feature-flagged):
  - **GPU** — raw OpenGL via EGL (Wayland), simple glyph shader. No `wgpu`,
    no `gl` crate; raw `unsafe` GL calls via our own thin loader. Deferred to M7.
  - **Software** — CPU framebuffer fallback for headless/CI use
- Render pipeline:
  1. Background fill (per-cell colors)
  2. Glyph batch (one draw call per atlas page)
  3. Cursor overlay
  4. Selection highlight
  5. GUI chrome (title bar, scrollbar, overlays via `text_layout`)
- Frame delivery: completed pixel buffer submitted as a Wayland `wl_buffer`
  commit via `quetzal-uring` write SQEs (no blocking Wayland socket writes).

**Deps (intra-workspace):** `crates/font`, `crates/grid`, `crates/text_layout`.

---

### `crates/platform`

Native window creation and event loop. Built on and extended from the `paint`
project scaffold (`/home/cuarentaydos/Code/forfun/paint`).

`paint` already has:
- Raw Wayland wire protocol (no `smithay-client-toolkit`)
- `io_uring`-based I/O via `ququmatz`
- SPSC/MPSC channels via `quetzalcoatl`
- Software framebuffer to shared memory surface

We extend it with:
- XDG toplevel window management (resize, fullscreen, move)
- EGL surface creation for GPU rendering (`crates/renderer`)
- Clipboard: `wl_data_device`
- DPI / scale factor detection

**I/O model:** the platform-thread owns a `quetzal-uring` `UringQueue` (using
the `demux::Router` drive model when Wayland + PTY + inotify fds all need to be
multiplexed). Each fd gets its own shard; completions route by `user_data` tag.
Wayland socket reads arrive as CQEs on the platform shard; decoded input events
are pushed onto an MPSC `quetzalcoatl` ring towards `crates/input`. Outgoing
Wayland frames are submitted as write SQEs on the renderer shard.

**Deps (tooling):** `ququmatz`, `quetzal-uring`, `quetzalcoatl`.

---

### `crates/input`

Translates raw platform keyboard/mouse events into terminal input byte
sequences.

- **Keyboard:**
  - Standard VT sequences (`\x1b[A` etc.)
  - Kitty keyboard protocol (extended key reporting)
  - `xterm` modifyOtherKeys
  - IME composition events
- **Mouse:**
  - X10 / normal / button-event / any-event tracking modes
  - SGR mouse extension (large coordinates)
  - Scroll → `\x1b[5~` / `\x1b[6~` or smooth pixel delta
- Keybind layer: user-defined actions (copy, paste, new tab, font-size±)

**Threading:** input-thread receives raw `InputEvent`s from the platform-thread
via an MPSC `quetzalcoatl` ring (multiple Wayland/X11 event types fan into one
consumer), encodes them to byte sequences, and pushes the results onto an SPSC
ring towards the pty-thread's write path.

**Deps:** `quetzalcoatl` (MPSC in, SPSC out).

---

### `crates/config`

TOML configuration file parser and live-reload watcher.

Config covers:
- Font: family, size, fallback chain
- Colors: 16 ANSI palette + background/foreground/cursor
- Shell: path, args, env overrides
- Keybinds: action → key combo map
- Renderer: GPU vs software, vsync, padding
- Scrollback: max lines

**Parser:** hand-written recursive-descent TOML parser (or pull from `tommy` if
it exists in the tooling workspace — TBD).

**Live reload:**
- `ququmatz::Inotify` watches the config file with `IN_MODIFY`
- `Sqe::read(inotify.fd(), …)` submitted to the shared `io_uring`
- CQE triggers re-parse on the config-thread
- New `Arc<Config>` published to all threads via a `quetzalcoatl::broadcast`
  ring — each thread subscribes once at startup and polls the ring on every
  iteration of its loop, no locks required

**Deps (tooling):** `ququmatz` (`Inotify`), `quetzalcoatl` (broadcast ring).

---

### `crates/tlog`

Compile-time-gated, runtime-selectable tracing, replacing one bespoke
`std::env::var("TERMINAL_DEBUG_X")` check per subsystem with a single facility
every crate can use.

- **Compile time:** the `trace` Cargo feature. Off (the default, and what
  every release/normal build uses): every `tlog::trace!` call site compiles to
  nothing but a type-check of its format arguments — no env read, no branch,
  no call.
- **Runtime:** once compiled in, `TERMINAL_LOG=pty,render` (or `=all`) picks
  which named `Channel`s (`pty`, `parser`, `grid`, `render`, `input`,
  `platform`, `config`, `fps`) actually emit. An unselected channel costs one
  atomic load; its `format_args!` is never evaluated.
- Output goes to stderr, or to a file named by `TERMINAL_LOG_FILE` — handy for
  high-volume channels like raw PTY bytes, which would otherwise interleave
  with the very terminal being debugged. `tlog::Escaped` renders control bytes
  visibly (`ESC` → `\e`, etc.) for exactly that use.

Deliberately separate from `TERMINAL_DEBUG_FPS` (`crates/platform/src/window.rs`):
that's an already-zero-cost, documented on-screen overlay with its own
established workflow (`scripts/stress/README.md`) and doesn't fit the
"named channel, formatted line" shape here.

**Deps:** none.

---

## Data-flow overview

```
Shell process
    │  (slave PTY fd)
    ▼
┌─────────┐  io_uring read CQEs   ┌────────────┐  spsc TermEvents  ┌──────┐
│   pty   │──────────────────────►│ vt_parser  │──────────────────►│ grid │
│ thread  │                       │   thread   │                   │thread│
└────▲────┘                       └────────────┘                   └──┬───┘
     │  io_uring write SQEs                                            │ spsc DirtyRegion
     │                                                                 ▼
┌────┴────┐  spsc encoded bytes                            ┌──────────────────┐
│  input  │◄───────────────────────────────────────────────│ renderer thread  │
│ thread  │                                                │  (font + grid)   │
└────▲────┘                                                └────────┬─────────┘
     │  mpsc InputEvents                                             │ io_uring write SQEs
     │                                                               │ (Wayland frames)
┌────┴─────────────┐                                                 │
│ platform thread  │◄────────────────────────────────────────────────┘
│ (Wayland / X11)  │
└────▲─────────────┘
     │
     │  broadcast Arc<Config>
┌────┴──────┐
│  config   │◄── ququmatz Inotify ── config file
│  thread   │
└───────────┘
```

---

## External scaffolding to reuse

| Source project | Location | What we take |
|----------------|----------|--------------|
| `paint` | `/home/cuarentaydos/Code/forfun/paint` | Wayland wire protocol, `io_uring` I/O, software framebuffer → `crates/platform` base |
| `ququmatz` | `/home/cuarentaydos/Code/tooling/ququmatz` | `io_uring` SQE builders, `Inotify`, `EventFd`, raw fd types |
| `quetzal-uring` | `/home/cuarentaydos/Code/tooling/quetzalcoatl-uring` | Lock-free SQ/CQ wrappers, `demux::Router` for fd multiplexing |
| `quetzalcoatl` | `/home/cuarentaydos/Code/tooling/quetzalcoatl` | All intra-process ring topologies (SPSC, MPSC, broadcast) |
| `tommy` | TBD (tooling workspace, not yet located) | TOML parser → `crates/config` internals |
| `pretext` | https://github.com/chenglou/pretext | Text layout algorithm → `crates/text_layout` |

---

## Build order / dependency graph

```
quetzalcoatl  ququmatz  quetzal-uring   (tooling — path deps)
      │            │           │
      └────────────┴───────────┘
                   │
      ┌────────────┼─────────────────┐
      │            │                 │
   config         pty           vt_parser
                                    │
                                   grid
                                    │
                              text_layout
                                    │
                                  font
                                    │
                               renderer
                                    │
                    ┌───────────────┤
                    │               │
                platform          input
                    │               │
                    └───────────────┘
                            │
                     bins/terminal
```

Crates with no intra-workspace deps (`pty`, `vt_parser`, `config`,
`text_layout`) can be developed and tested in complete isolation first.

---

## Milestones

### M0 — Workspace scaffold ✅
- [x] Workspace `Cargo.toml` with lint config
- [x] `bins/terminal` stub compiles clean

### M1 — PTY + shell spawning
- [ ] `crates/pty`: `forkpty` wrapper
- [ ] Master fd registered with `quetzal-uring` `UringQueue`
- [ ] Read CQEs → SPSC ring (raw bytes visible on stdout for debugging)
- [ ] Resize via `TIOCSWINSZ` ioctl
- [ ] `SIGCHLD` via `signalfd` + io_uring read SQE

### M2 — VT parser
- [ ] `crates/vt_parser`: state machine, all major sequence classes
- [ ] Consumes SPSC byte ring from M1, produces SPSC `TermEvent` ring
- [ ] Fuzz corpus against known-good terminal output

### M3 — Grid
- [ ] `crates/grid`: cells, cursor, scrollback, dirty tracking
- [ ] Consumes `TermEvent` SPSC ring from M2
- [ ] Headless render-to-string for testing (`Display` impl)

### M4 — Platform window
- [ ] Extend `paint` into `crates/platform`
- [ ] `quetzal-uring` `demux::Router` multiplexing Wayland + PTY fds
- [ ] Wayland window with software framebuffer
- [ ] Input events → MPSC `quetzalcoatl` ring → `crates/input`

### M5 — Input
- [ ] `crates/input`: MPSC consumer, encode to VT byte sequences
- [ ] Push encoded bytes onto SPSC ring → pty-thread write SQEs
- [ ] Keyboard, mouse, keybinds

### M6 — Font + renderer (software)
- [ ] `crates/font`: TTF parse, rasterise, atlas
- [ ] `crates/renderer`: software backend, consume dirty-region SPSC ring
- [ ] Visible text in window 🎉

### M7 — GPU renderer
- [ ] EGL setup, GL context
- [ ] Glyph shader, batch draw calls
- [ ] `crates/renderer`: GPU backend behind feature flag
- [ ] Frame delivery via `quetzal-uring` write SQEs

### M8 — Text layout + GUI chrome
- [ ] `crates/text_layout`: pretext port (analysis → measurement → line_break → layout → bidi)
- [ ] Title bar, scrollbar, overlays

### M9 — Config
- [ ] `crates/config`: TOML parse (hand-written or `tommy`)
- [ ] `ququmatz::Inotify` + io_uring read SQE for live reload
- [ ] `quetzalcoatl::broadcast` ring publishing `Arc<Config>` to all threads

### M10 — Polish
- [ ] Sixel graphics
- [ ] Kitty image protocol
- [ ] Ligatures (via `crates/font` GSUB table parsing)
- [ ] Tabs / multiplexing
