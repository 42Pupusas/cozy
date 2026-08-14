//! `terminal` — multi-threaded entry point.
//!
//! Thread topology (M8):
//!
//! ```text
//!  ┌────────────┐  MPSC<ParserMsg>  ┌──────────────┐  SPSC<DirtySnapshot>  ┌──────────────┐
//!  │ PtyWorker  │ ────────────────► │ ParserWorker  │ ─────────────────────► │  UiWorker    │
//!  │            │  (Data carries    │ (vt_parser +  │                        │  (Wayland    │
//!  │ forkpty    │ ◄──────────────── │  grid)        │                        │   window)    │
//!  └────────────┘  MPSC<InputMsg>  └──────────────┘                        └──────┬───────┘
//!                  (key bytes /                                                     │
//!                   resize / quit)                                    SPSC<InputMsg>│
//!                                                                                  ▼
//!                                                                    PtyWorker input drain
//! ```
//!
//! Each box above is a worker struct (`PtyWorker`, `ParserWorker`,
//! `UiWorker`) that owns its ring endpoints as fields and is run on its own
//! OS thread (`UiWorker` runs on the main thread) -- not a free function
//! taking half a dozen loose channel arguments. `UiWorker` itself is a
//! plain data bag (see its doc): `UiLoop::new` consumes it by value to
//! build the loop's mutable state, and `UiLoop::run` is the actual `run`
//! method, so the dependency between the two runs one way only.
//!
//! Rings are quetzalcoatl SPSC/MPSC (MPSC where two threads fan in: pty+ui
//! into the parser, ui+parser into the pty) — no mutexes, no async runtime.
//! A fourth, unlabeled SPSC ring (parser → ui, [`UiEvent`]s: bell/title/
//! link/clipboard) runs alongside the `DirtySnapshot` one in the diagram
//! above; both are exactly one producer to one consumer, so neither needs
//! anything heavier than the lock-free SPSC ring every other channel here
//! uses.
//! The pty→parser `Data` message carries its 4 KiB read buffer inline, so the
//! ring's slots are the buffer pool: no per-chunk heap allocation and no
//! free-list recycle ring.
//! Config is loaded from `$XDG_CONFIG_HOME/terminal/config.toml` at startup.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

use config::Config;
use grid::{DirtySnapshot, Grid};
use input::{Encoder, Key, KeyState, KeyboardMode, Modifiers, MouseButton, MouseEvent, MouseMode};
use platform::{InputEvent, TermWindow};
use pty::{Pty, PtyInput, WinSize};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::RingBuffer;
use vt_parser::Parser;

mod headless;

/// The grid's negotiated terminal modes, packed into one `u32` for cheap
/// lock-free cross-thread transport (parser thread writes after every grid
/// batch, UI thread reads before encoding each key/mouse event) -- the same
/// pattern `bracketed_paste` already uses via a plain `AtomicBool`, extended
/// to more than one bit of state.
///
/// Bit layout (low to high): `mouse_mode` (3 bits, [`MouseMode::to_u8`]),
/// `sgr_mouse` (1 bit), `keyboard_mode` (2 bits, [`KeyboardMode::to_u8`]),
/// `kitty_flags` (8 bits), `app_cursor` (1 bit), `app_keypad` (1 bit),
/// `focus_events` (1 bit), `alt_screen` (1 bit).
#[allow(clippy::struct_excessive_bools)] // each bool is an independent negotiated flag, not a state machine
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct TermModes {
    mouse_mode: MouseMode,
    sgr_mouse: bool,
    keyboard_mode: KeyboardMode,
    kitty_flags: u8,
    app_cursor: bool,
    app_keypad: bool,
    focus_events: bool,
    /// Whether the alternate screen is active. Routes the mouse wheel: no
    /// scrollback exists there, so with no app mouse tracking the wheel is
    /// translated to arrow keys ("alternate scroll", what foot/kitty do) so
    /// full-screen apps like `less` still scroll.
    alt_screen: bool,
}

impl TermModes {
    fn pack(self) -> u32 {
        let mut v = u32::from(self.mouse_mode.to_u8());
        v |= u32::from(self.sgr_mouse) << 3;
        v |= u32::from(self.keyboard_mode.to_u8()) << 4;
        v |= u32::from(self.kitty_flags) << 6;
        v |= u32::from(self.app_cursor) << 14;
        v |= u32::from(self.app_keypad) << 15;
        v |= u32::from(self.focus_events) << 16;
        v |= u32::from(self.alt_screen) << 17;
        v
    }

    const fn unpack(v: u32) -> Self {
        Self {
            mouse_mode: MouseMode::from_u8((v & 0b111) as u8),
            sgr_mouse: (v >> 3) & 1 != 0,
            keyboard_mode: KeyboardMode::from_u8(((v >> 4) & 0b11) as u8),
            kitty_flags: ((v >> 6) & 0xFF) as u8,
            app_cursor: (v >> 14) & 1 != 0,
            app_keypad: (v >> 15) & 1 != 0,
            focus_events: (v >> 16) & 1 != 0,
            alt_screen: (v >> 17) & 1 != 0,
        }
    }

    /// Snapshot the grid's current negotiated modes (called on the parser
    /// thread, which owns the grid).
    fn from_grid(grid: &Grid) -> Self {
        Self {
            mouse_mode: grid.mouse_mode(),
            sgr_mouse: grid.sgr_mouse(),
            keyboard_mode: grid.keyboard_mode(),
            kitty_flags: grid.kitty_flags(),
            app_cursor: grid.app_cursor(),
            app_keypad: grid.app_keypad(),
            focus_events: grid.focus_events(),
            alt_screen: grid.alt_screen_active(),
        }
    }

    /// Apply to an `Encoder` (called on the UI thread before encoding).
    const fn apply_to(self, enc: &mut Encoder) {
        enc.mouse_mode = self.mouse_mode;
        enc.sgr_mouse = self.sgr_mouse;
        enc.keyboard_mode = self.keyboard_mode;
        enc.kitty_flags = self.kitty_flags;
        enc.app_cursor = self.app_cursor;
        enc.app_keypad = self.app_keypad;
    }
}

/// Initial terminal dimensions (cells).
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// PTY read buffer size.
const PTY_BUF: usize = 4096;

// A PTY read chunk must fit in one `ParserMsg::Data` slot: `Pty::run`
// retries the *whole* rejected buffer when `on_output` returns false, so
// the push must be all-or-nothing — splitting one read across several ring
// slots would deliver the already-pushed parts twice on retry.
const _: () = assert!(
    pty::READ_BUF_SIZE <= PTY_BUF,
    "a PTY read chunk must fit in one ParserMsg::Data slot"
);

// Ring topology (3 rings, one per directed edge of the pty→parser→ui→pty
// cycle -- the data ring carries its read buffer inline, so no separate
// free-list ring is needed to recycle buffers parser→pty):
//
//   parser_in  (ParserMsg)    pty + ui  → parser   MPSC  data chunks + resizes
//   dirty      (DirtySnapshot) parser   → ui       SPSC  render snapshots
//   input      (InputMsg)      ui + parser → pty    MPSC  keystrokes + CPR replies
//
// The pty→parser data ring carries its 4 KiB read buffer *inline* in each
// `ParserMsg::Data` slot (not behind a `Box`), so the ring's own slots are
// the buffer pool: there is no separate free-list ring recycling spent
// buffers, and no per-chunk heap allocation. A single 4 KiB copy into the
// slot on push replaces the old `Box` indirection + free-list round-trip;
// measured (benches/tty transport) as end-to-end noise once the parser+grid
// work that rides on every chunk is accounted for.
//
/// Ring capacities (power-of-two items).
const RING_PARSER_IN: usize = 64; // ParserMsg slots     pty+ui → parser
const RING_DIRTY: usize = 16; // DirtySnapshot slots parser → ui
const RING_INPUT: usize = 64; // InputMsg slots      ui+parser → pty
/// `UiEvent` slots, parser → ui. Small: these are one-shot, edge-triggered
/// events (bell/title/link/clipboard), at most one per OSC/BEL the shell
/// emits, never per-byte -- nothing like the PTY data rate the other rings
/// carry.
const RING_UI_EVENTS: usize = 32;

/// Cap on how many `ParserMsg`s the parser thread folds into one grid batch
/// before publishing a dirty snapshot and re-entering the loop. Without this,
/// a flood of PTY output (e.g. `yes`, a big `cat`) keeps the parser thread
/// feeding the grid for as long as data keeps arriving, and the renderer
/// never sees an intermediate frame -- output looks stalled, then jumps.
/// Re-checking every 64 chunks keeps streaming output smooth without giving
/// up meaningful throughput (each chunk is up to 4 KiB).
const BATCH_CHUNK_CAP: usize = 64;

/// Time-based half of the batch cap: publish a snapshot at least this often
/// even if `BATCH_CHUNK_CAP` hasn't been hit yet (e.g. chunks arriving just
/// slowly enough to never trip the count cap but fast enough to starve the
/// renderer for a while). Checked every chunk via `Instant::now()`, which is
/// cheap (vDSO) relative to a single parse+grid-apply of up to 4 KiB.
const BATCH_TIME_CAP: Duration = Duration::from_millis(5);

// ── Messages ──────────────────────────────────────────────────────────────────

/// A raw PTY read chunk — the buffer lives *inline* in the ring slot, not
/// behind a `Box`. This makes the data ring its own buffer pool (no
/// free-list ring, no per-chunk allocation); the tradeoff is a single 4 KiB
/// copy into the slot on push, which the pty thread pays in `on_output`.
/// `len` bytes of `buf` are valid; the tail is unspecified padding.
#[derive(Clone)]
struct PtyChunk {
    buf: [u8; PTY_BUF],
    len: usize,
}

/// Messages into the parser/grid thread. Merged onto one MPSC ring: the pty
/// thread sends shell output (`Data`), the ui thread sends window resizes
/// (`Resize`) so the grid tracks the new cell extent and reflows.
//
// `Data` is deliberately large: it carries the 4 KiB read buffer inline so
// the ring slot doubles as the buffer pool (see the topology note above).
// Boxing it — clippy's usual suggestion — would re-introduce the per-chunk
// heap indirection and free-list recycle ring this design exists to delete,
// and the size disparity is harmless here: the ring is a fixed 64 slots, so
// the whole buffer is ~263 KiB allocated once at startup, and `Resize`/
// `Scroll` are rare relative to `Data`.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum ParserMsg {
    Data(PtyChunk),
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Scroll the viewport by `delta` lines (positive = up into scrollback).
    Scroll {
        delta: i32,
    },
}

/// Messages from the UI thread to the PTY thread.
#[derive(Clone)]
enum InputMsg {
    Bytes(Box<[u8]>),
    Resize { cols: u16, rows: u16 },
    Quit,
}

/// Rare, one-shot UI-facing events the grid produces that don't fit the
/// polled-state pattern `bracketed_paste`/`TermModes` use (those are level-
/// triggered: the UI thread just re-reads the latest value every batch). A
/// bell or a title change is edge-triggered -- it must be observed exactly
/// once, not re-applied every batch -- so it needs a real queue instead of
/// an atomic.
///
/// Carried over a single-producer/single-consumer `quetzalcoatl::spsc`
/// ring (parser thread produces, UI thread consumes) -- see
/// [`RING_UI_EVENTS`] -- matching the lock-free-everywhere contract every
/// other cross-thread channel in this file already follows (`ParserMsg`,
/// `InputMsg`, `DirtySnapshot`). This used to be an `Arc<Mutex<Vec<_>>>`,
/// the one lock in an otherwise lock-free pipeline: exactly one producer
/// and exactly one consumer, so a lock bought nothing a lock-free SPSC ring
/// doesn't already give for free, and it meant a panic on either side while
/// holding the lock would poison it and wedge the other thread's next
/// access too.
enum UiEvent {
    Bell,
    Title(String),
    /// A newly-interned OSC 8 hyperlink: `(link_id, uri)`. The UI thread
    /// mirrors these into its own id->URI table so a click handler never has
    /// to reach across to the parser-thread-owned `Grid` to resolve one.
    Link(u32, String),
    /// A decoded OSC 52 clipboard write (`ESC ] 52 ; c ; <base64> BEL`) --
    /// the application asked to put this text on the system clipboard.
    Clipboard(String),
    /// An OSC 4/104 palette change: `(index, rgb)`. Applied to the live
    /// renderer's theme and forces a full repaint -- any on-screen cell
    /// using this index now paints a different color.
    PaletteColor(u8, (u8, u8, u8)),
}

/// Stores `true` into a shared quit flag when dropped -- including when the
/// stack is unwinding from a panic, not just on a normal return.
///
/// Without this, a panic on the PTY or parser thread (a bug, an unexpected
/// `unwrap`/index-out-of-bounds, whatever) skips straight past that thread's
/// own `quit.store(true, ...)` tail (it never runs -- the unwind jumps clean
/// over it), so the *other* threads never learn anything died. They just
/// keep polling a `quit` flag that's permanently stuck at `false`: the
/// window stops updating, the shell stops echoing, and the process sits
/// there looking hung forever instead of exiting with the panic message
/// already printed to stderr by the default hook. Held for the whole
/// duration of a worker's `run`, this guarantees the flag flips regardless
/// of *how* that function stops running.
struct QuitGuard(Arc<AtomicBool>);

impl Drop for QuitGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

// ── Entry ──────────────────────────────────────────────────────────────────────

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match headless::TerminalStress::maybe_run(&arguments) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("terminal: {error}");
            std::process::exit(1);
        }
    }
    let cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("terminal: config error: {e} — using defaults");
        Config::default()
    });

    let cli = Cli::parse(std::env::args().skip(1));

    if let Err(e) = App::new(&cfg, cli).run() {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    }
}

/// Command-line arguments this binary understands. Currently just `-e`/
/// `--exec`, xterm/foot/alacritty's convention for "run this command instead
/// of the configured shell, and exit the terminal window when it exits" --
/// the mechanism a `.desktop` file's `Terminal=true` entry (e.g. `nvim`,
/// `htop`) needs a terminal to launch it *through*, rather than launching
/// the TUI program directly with no controlling terminal at all.
#[derive(Clone)]
struct Cli {
    /// `Some((program, args))` when `-e`/`--exec` was given; `None` runs the
    /// configured shell as normal. Everything after the flag is consumed
    /// verbatim as the command's own argv (not re-parsed for further terminal
    /// flags), matching how xterm/foot/alacritty's `-e` works: it must be the
    /// last thing on the command line.
    exec: Option<(String, Vec<String>)>,
}

impl Cli {
    /// Parse an argv iterator (already stripped of `argv[0]`). Unrecognized
    /// leading arguments are ignored rather than erroring -- this binary has
    /// no other flags yet, and silently ignoring stray tokens is friendlier
    /// to callers (window managers, desktop files) than aborting startup.
    fn parse<I: Iterator<Item = String>>(mut args: I) -> Self {
        while let Some(arg) = args.next() {
            if arg == "-e" || arg == "--exec" {
                if let Some(program) = args.next() {
                    return Self {
                        exec: Some((program, args.collect())),
                    };
                }
                return Self { exec: None };
            }
        }
        Self { exec: None }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::Cli;

    fn args(items: &[&str]) -> impl Iterator<Item = String> + use<> {
        items
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn no_flags_runs_default_shell() {
        let cli = Cli::parse(args(&[]));
        assert!(cli.exec.is_none());
    }

    #[test]
    fn dash_e_with_program_only() {
        let cli = Cli::parse(args(&["-e", "nvim"]));
        assert_eq!(cli.exec, Some(("nvim".to_owned(), vec![])));
    }

    #[test]
    fn dash_e_with_program_and_args() {
        let cli = Cli::parse(args(&["-e", "nvim", "--", "foo.txt"]));
        assert_eq!(
            cli.exec,
            Some((
                "nvim".to_owned(),
                vec!["--".to_owned(), "foo.txt".to_owned()]
            ))
        );
    }

    #[test]
    fn long_flag_form_also_works() {
        let cli = Cli::parse(args(&["--exec", "htop"]));
        assert_eq!(cli.exec, Some(("htop".to_owned(), vec![])));
    }

    #[test]
    fn dash_e_with_no_program_falls_back_to_default_shell() {
        let cli = Cli::parse(args(&["-e"]));
        assert!(cli.exec.is_none());
    }

    #[test]
    fn unrecognized_leading_args_are_ignored() {
        let cli = Cli::parse(args(&["--some-future-flag", "-e", "nvim"]));
        assert_eq!(cli.exec, Some(("nvim".to_owned(), vec![])));
    }
}

/// The application itself: the composition root that owns the process-wide
/// shared state spanning all three worker threads and drives the whole
/// lifecycle (wire rings → spawn workers → run the UI on this thread → join).
///
/// Was a free `run(cfg)` function -- the one part of the topology that wasn't
/// yet an object even though every thread already is (`PtyWorker`,
/// `ParserWorker`, `UiWorker`). The rings are transient wiring built and
/// consumed within [`run`], so they stay locals; the two `Arc<AtomicBool>`s
/// are genuinely app-wide state (every thread reads or writes them), so they
/// are fields.
struct App<'cfg> {
    cfg: &'cfg Config,
    /// `-e`/`--exec` override from the command line: run this instead of
    /// `cfg.shell`. See [`Cli`]'s doc for why a terminal needs this at all.
    cli: Cli,
    /// Set once by whichever thread first decides to exit; every worker loop
    /// polls it to unwind. Cloned into each worker.
    quit: Arc<AtomicBool>,
    /// Bracketed-paste mode (DECSET 2004): written on the parser thread
    /// (owns the grid), read on the UI thread (wraps clipboard paste).
    bracketed_paste: Arc<AtomicBool>,
    /// The grid's other negotiated terminal modes (mouse tracking/SGR,
    /// keyboard protocol, app-cursor/keypad, focus events), packed via
    /// [`TermModes::pack`]: written on the parser thread after every grid
    /// batch, read on the UI thread before encoding each key/mouse event.
    term_modes: Arc<AtomicU32>,
    /// PID of the forked shell child, `0` until the PTY thread finishes
    /// spawning it. Lets the UI thread implement "open a new terminal here"
    /// (Ctrl+Shift+Enter) by resolving `/proc/<pid>/cwd` directly -- no
    /// window-manager IPC or external script needed.
    shell_pid: Arc<AtomicI32>,
}

impl<'cfg> App<'cfg> {
    fn new(cfg: &'cfg Config, cli: Cli) -> Self {
        Self {
            cfg,
            cli,
            quit: Arc::new(AtomicBool::new(false)),
            bracketed_paste: Arc::new(AtomicBool::new(false)),
            term_modes: Arc::new(AtomicU32::new(0)),
            shell_pid: Arc::new(AtomicI32::new(0)),
        }
    }

    fn run(&self) -> platform::Result<()> {
        // Block SIGCHLD before any thread exists so every thread inherits the
        // mask — the PTY thread's signalfd only sees *blocked* signals, and an
        // unblocked SIGCHLD delivered to some other thread would be lost.
        if let Err(e) = pty::RawPty::block_sigchld() {
            eprintln!("terminal: sigprocmask failed: {e} — child exit may not be detected");
        }

        // ── Rings ───────────────────────────────────────────────────────────────
        // parser_in is MPSC: the pty thread pushes shell output, the ui thread
        // pushes resizes. The ring's own park/unpark wakes the parked parser when
        // either producer pushes — so resizes wake the grid directly (no eventfd).
        let (parser_data_tx, parser_in_rx) =
            mpsc::RingBuffer::<ParserMsg>::new(Capacity::exact(RING_PARSER_IN)).split();
        let parser_resize_tx = parser_data_tx.clone();
        let (dirty_tx, dirty_rx) =
            RingBuffer::<DirtySnapshot>::new(Capacity::exact(RING_DIRTY)).split();
        // ui_events is SPSC: exactly one producer (parser thread) and one
        // consumer (UI thread) -- see `UiEvent`'s doc for why this replaced
        // an `Arc<Mutex<Vec<_>>>`.
        let (ui_events_tx, ui_events_rx) =
            RingBuffer::<UiEvent>::new(Capacity::exact(RING_UI_EVENTS)).split();
        // input is MPSC too: the ui thread (keystrokes / resize / quit) and the
        // parser/grid thread (CPR/DA responses written back to the shell) both feed
        // the single PTY consumer.
        let (input_tx, input_rx) =
            mpsc::RingBuffer::<InputMsg>::new(Capacity::exact(RING_INPUT)).split();
        let reply_tx = input_tx.clone();

        // ── Shared, app-owned cross-thread state ─────────────────────────────────
        // `quit` and `bracketed_paste` are fields of the App (they outlive any one
        // thread and every thread touches them); each worker gets its own cloned
        // handle. See the struct doc for who reads vs. writes each.
        let quit_pty = Arc::clone(&self.quit);
        let quit_parser = Arc::clone(&self.quit);
        let shell_pid_pty = Arc::clone(&self.shell_pid);
        let shell_pid_ui = Arc::clone(&self.shell_pid);
        let bracketed_paste_parser = Arc::clone(&self.bracketed_paste);
        let bracketed_paste_ui = Arc::clone(&self.bracketed_paste);
        let term_modes_parser = Arc::clone(&self.term_modes);
        let term_modes_ui = Arc::clone(&self.term_modes);

        // ── Cross-thread wakeups (eventfd + poll) ────────────────────────────────
        // Each thread that waits on more than one source blocks in poll(2) on an
        // eventfd instead of busy-spinning. `ui_wake` is owned by the window (the
        // Wayland reader signals it); the parser also signals it after a snapshot.
        // `pty_wake` lets the UI thread rouse the PTY thread when it queues input.
        let ui_wake = platform::Waker::new().expect("create ui waker");
        let pty_wake = waker::Waker::new().expect("create pty waker");
        let parser_ui_wake = ui_wake.clone();
        let parser_pty_wake = pty_wake.clone();
        let ui_pty_wake = pty_wake.clone();
        // Kept for the always-run teardown after the UI loop returns (see
        // below) -- `pty_wake` itself is moved into `PtyWorker` shortly.
        let shutdown_pty_wake = pty_wake.clone();

        // ── Spawn PTY thread ────────────────────────────────────────────────────
        // `-e`/`--exec` replaces the configured shell wholesale (program +
        // its own argv) rather than being appended to it -- exactly what
        // xterm/foot/alacritty's `-e` does, and what a `.desktop` entry's
        // `Terminal=true` `Exec=` line needs: run *that* command, with the
        // shell nowhere in the picture.
        let (shell, shell_args): (String, Vec<String>) = match self.cli.exec.clone() {
            Some((program, args)) => (program, args),
            None => (self.cfg.shell.clone(), Vec::new()),
        };
        let pty_handle = PtyWorker {
            shell,
            shell_args,
            parser_tx: parser_data_tx,
            input_rx,
            input_wake: pty_wake,
            quit: quit_pty,
            shell_pid: shell_pid_pty,
        }
        .spawn();

        // ── Spawn parser + grid thread ──────────────────────────────────────────
        let parser_handle = ParserWorker {
            cols: COLS,
            rows: ROWS,
            default_fg: grid::Color::Rgb(
                self.cfg.palette.foreground.r,
                self.cfg.palette.foreground.g,
                self.cfg.palette.foreground.b,
            ),
            default_bg: grid::Color::Rgb(
                self.cfg.palette.background.r,
                self.cfg.palette.background.g,
                self.cfg.palette.background.b,
            ),
            parser_rx: parser_in_rx,
            dirty_tx,
            reply_tx,
            ui_wake: parser_ui_wake,
            pty_wake: parser_pty_wake,
            bracketed_paste: bracketed_paste_parser,
            term_modes: term_modes_parser,
            ui_events_tx,
            quit: quit_parser,
        }
        .spawn();

        // ── UI thread (main thread) ─────────────────────────────────────────────
        // A shutdown handle kept *outside* the UiWorker: whatever happens to
        // the UI loop (clean quit, or an early `Err` return e.g. the
        // compositor tearing down the Wayland connection on Alt+F4 instead
        // of sending a polite `xdg_toplevel.close`), this lets us always
        // signal the PTY thread to hang up the shell and join every worker
        // before the process exits. Without this, an error return from
        // `UiWorker::run` used to skip straight past the `quit`/join tail
        // below via `?`, leaking the forked shell as an orphan: `main`
        // exiting kills this process's *threads*, but the shell is a
        // separate forked *process*, so nothing ever reaped or SIGHUP'd it.
        let shutdown_input_tx = input_tx.clone();

        // Same teardown guarantee as `QuitGuard` (see its doc), applied here
        // via `catch_unwind` instead of `Drop` because this is the main
        // thread itself: an *unhandled* panic on `main` normally unwinds
        // straight past any code sequentially after the panicking call --
        // including the quit-flag/join tail below -- and terminates the
        // process without ever signalling the PTY thread. That leaves the
        // forked shell running as an orphan exactly like the `?`-propagation
        // bug this replaced, just triggered by a panic instead of an `Err`
        // return. Catching it here, tearing down regardless, then
        // re-throwing via `resume_unwind` preserves the original panic
        // (message, exit code, backtrace) while still guaranteeing cleanup.
        let ui_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let worker = UiWorker {
                cfg: self.cfg,
                dirty_rx,
                input_tx,
                resize_tx: parser_resize_tx.clone(),
                scroll_tx: parser_resize_tx,
                ui_wake,
                pty_wake: ui_pty_wake,
                bracketed_paste: &bracketed_paste_ui,
                term_modes: &term_modes_ui,
                ui_events_rx,
                quit: &self.quit,
                shell_pid: &shell_pid_ui,
            };
            UiLoop::new(worker).and_then(UiLoop::run)
        }));

        // Always tear down, regardless of whether the UI loop exited
        // cleanly, with an error, or by panicking: set the shared quit flag,
        // push a Quit message the PTY thread's `next_input` will observe,
        // and wake it in case it's parked in `io_uring_enter`.
        self.quit.store(true, Ordering::Relaxed);
        let _ = shutdown_input_tx.push(InputMsg::Quit);
        shutdown_pty_wake.signal();

        pty_handle.join().ok();
        parser_handle.join().ok();

        match ui_outcome {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
}

// ── PTY thread ────────────────────────────────────────────────────────────────

/// Owns every ring endpoint the PTY-facing thread touches, so a thread entry
/// point that used to be a six-argument free function is now just "build one
/// of these, then run it".
///
/// Thread entry points must *own* their ring endpoints and the quit `Arc`:
/// the fields are moved into a `'static` closure by `spawn`, and dropping the
/// producer/consumer on thread exit is what closes the ring. Borrowing is
/// structurally impossible.
struct PtyWorker {
    shell: String,
    /// Argv for `shell` beyond argv[0] -- empty for the default login shell,
    /// populated when `-e`/`--exec` named a command with its own arguments.
    shell_args: Vec<String>,
    parser_tx: mpsc::Producer<ParserMsg>,
    input_rx: mpsc::Consumer<InputMsg>,
    input_wake: waker::Waker,
    quit: Arc<AtomicBool>,
    /// Written once, right after the shell is forked -- see the field doc
    /// on `App::shell_pid`.
    shell_pid: Arc<AtomicI32>,
}

impl PtyWorker {
    /// Spawn the PTY event loop on its own OS thread, consuming `self`.
    fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("pty".into())
            .spawn(move || self.run())
            .expect("spawn pty thread")
    }

    fn run(self) {
        let Self {
            shell,
            shell_args,
            parser_tx,
            mut input_rx,
            input_wake,
            quit,
            shell_pid,
        } = self;
        // Guarantees `quit` flips even if this thread panics below -- see
        // `QuitGuard`'s doc.
        let _quit_guard = QuitGuard(Arc::clone(&quit));

        // All PTY I/O goes through io_uring: the master fd, the SIGCHLD
        // signalfd, and the input-wake eventfd each keep one read SQE armed,
        // and the loop parks in io_uring_enter until any of them completes —
        // no poll(2), no 100ms tick. The UI/parser threads signal
        // `input_wake` after pushing to `input_rx`, which completes the
        // eventfd SQE.
        let win = WinSize {
            rows: ROWS,
            cols: COLS,
            xpixel: 0,
            ypixel: 0,
        };
        let shell_args_ref: Vec<&str> = shell_args.iter().map(String::as_str).collect();
        let pty = match Pty::spawn(
            &shell,
            &shell_args_ref,
            win,
            input_wake.raw_fd(),
            Arc::clone(&quit),
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("pty: spawn failed: {e}");
                quit.store(true, Ordering::Relaxed);
                return;
            }
        };
        shell_pid.store(pty.child_pid(), Ordering::Relaxed);

        // Adapter: our MPSC ring messages → PtyInput. Quit also raises the
        // shared flag so the parser/UI threads stop too.
        let quit_in = Arc::clone(&quit);
        let next_input = move || {
            input_rx.pop().map(|msg| match msg {
                InputMsg::Bytes(b) => PtyInput::Bytes(b),
                InputMsg::Resize { cols, rows } => PtyInput::Resize(WinSize {
                    rows,
                    cols,
                    xpixel: 0,
                    ypixel: 0,
                }),
                InputMsg::Quit => {
                    quit_in.store(true, Ordering::Relaxed);
                    PtyInput::Quit
                }
            })
        };

        // Adapter: PTY output chunks → parser ring. `bytes` never exceeds
        // `pty::READ_BUF_SIZE`, which equals `PTY_BUF` (compile-time
        // asserted below), so a chunk always fits in exactly one ring slot.
        // This must NOT be a push-per-part loop: `Pty::run` retries the
        // *whole* rejected buffer on `false`, so partially pushing several
        // parts and then failing would deliver the pushed parts twice on
        // retry. All-or-nothing is the only retry-safe shape.
        //
        // The read bytes are copied *inline* into the `ParserMsg::Data` slot
        // (the ring slot is the buffer -- no `Box`, no free-list recycle
        // ring). One 4 KiB copy per chunk; the ring's fixed slots are reused
        // in place as the parser drains them.
        //
        // Real backpressure: returns `false` (telling `Pty::run` NOT to
        // re-arm the master-fd read) when the parser ring is full, instead
        // of silently dropping the chunk. The kernel's own pty output buffer
        // then fills up and throttles the child's `write(2)` calls for us.
        let on_output = move |bytes: &[u8]| -> bool {
            let mut buf = [0u8; PTY_BUF];
            buf[..bytes.len()].copy_from_slice(bytes);
            parser_tx
                .push(ParserMsg::Data(PtyChunk {
                    buf,
                    len: bytes.len(),
                }))
                .is_ok()
        };

        match pty.run(next_input, on_output) {
            Ok(_code) => {}
            Err(e) => eprintln!("pty: event loop failed: {e}"),
        }
        // Shell exited (or loop errored) — signal everyone to shut down.
        quit.store(true, Ordering::Relaxed);
    }
}

// ── Parser + grid thread ──────────────────────────────────────────────────────

/// The parser/grid thread's **spawn payload**: the ring endpoints, wakers and
/// shared flags the coordinator wires up on the main thread and hands over,
/// plus the startup geometry. Deliberately *not* the running state -- the
/// grid and VT parser are born on the parser thread inside [`run`], because
/// `Grid::new` eagerly allocates and zero-fills a ~38 MB scrollback buffer
/// that must not land on the main thread's startup path. Once `run` moves
/// this payload onto its thread it becomes a [`ParserSession`], which owns
/// everything and drives the loop.
///
/// Was a ten-argument free function (`parser_thread`) with a sibling free
/// helper (`feed_parser`).
struct ParserWorker {
    cols: u16,
    rows: u16,
    /// Resolved theme fg/bg, seeded into the grid at construction so it can
    /// answer OSC 10/11 queries. See [`Grid::set_default_colors`].
    default_fg: grid::Color,
    default_bg: grid::Color,
    parser_rx: mpsc::Consumer<ParserMsg>,
    dirty_tx: quetzalcoatl::spsc::Producer<DirtySnapshot>,
    reply_tx: mpsc::Producer<InputMsg>,
    ui_wake: waker::Waker,
    pty_wake: waker::Waker,
    bracketed_paste: Arc<AtomicBool>,
    term_modes: Arc<AtomicU32>,
    ui_events_tx: quetzalcoatl::spsc::Producer<UiEvent>,
    quit: Arc<AtomicBool>,
}

impl ParserWorker {
    /// Spawn the parser/grid event loop on its own OS thread, consuming `self`.
    fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("parser".into())
            .spawn(move || self.run())
            .expect("spawn parser thread")
    }

    /// Bring the running state to life *on the parser thread* and drive the
    /// loop to quit. The `Grid`/`Parser` allocation happens here, not in the
    /// coordinator that built `self`, so the big scrollback buffer never
    /// touches the main thread's startup path.
    fn run(self) {
        let Self {
            cols,
            rows,
            default_fg,
            default_bg,
            parser_rx,
            dirty_tx,
            reply_tx,
            ui_wake,
            pty_wake,
            bracketed_paste,
            term_modes,
            ui_events_tx,
            quit,
        } = self;
        // Guarantees `quit` flips even if this thread panics below -- see
        // `QuitGuard`'s doc.
        let _quit_guard = QuitGuard(Arc::clone(&quit));
        let mut grid = Grid::new(cols, rows);
        grid.set_default_colors(default_fg, default_bg);
        ParserSession {
            grid,
            parser: Parser::new(),
            parser_rx,
            dirty_tx,
            reply_tx,
            ui_wake,
            pty_wake,
            bracketed_paste,
            term_modes,
            ui_events_tx,
            quit,
        }
        .run();
    }
}

/// The parser/grid thread's **running state**: the [`Grid`] and VT [`Parser`]
/// it drives, alongside the ring endpoints, wakers and flags moved in from
/// the [`ParserWorker`] spawn payload. Because the grid and parser are now
/// fields, the per-message helpers (`feed`, `flush_replies`, `publish`) are
/// plain `&mut self` methods operating on `self.grid` / `self.parser` /
/// `self.dirty_tx` -- no state threaded through as arguments, and no method
/// that borrows `self` without using it.
struct ParserSession {
    grid: Grid,
    parser: Parser,
    parser_rx: mpsc::Consumer<ParserMsg>,
    dirty_tx: quetzalcoatl::spsc::Producer<DirtySnapshot>,
    reply_tx: mpsc::Producer<InputMsg>,
    ui_wake: waker::Waker,
    pty_wake: waker::Waker,
    bracketed_paste: Arc<AtomicBool>,
    term_modes: Arc<AtomicU32>,
    ui_events_tx: quetzalcoatl::spsc::Producer<UiEvent>,
    quit: Arc<AtomicBool>,
}

impl ParserSession {
    /// Feed one parser message to the grid, applying it immediately. Order
    /// matters: a Resize must take effect before any Data that follows it
    /// (the shell's post-SIGWINCH repaint must land on the already-reflowed
    /// grid), so resizes are NOT deferred or coalesced across interleaved
    /// data.
    ///
    /// The `Data` chunk carries its bytes inline; once parsed the chunk is
    /// simply dropped and its ring slot becomes free for the pty thread to
    /// write the next read into -- no buffer hand-back, because the ring
    /// slot *is* the buffer.
    fn feed(&mut self, msg: ParserMsg) {
        match msg {
            ParserMsg::Data(chunk) => {
                let bytes = &chunk.buf[..chunk.len];
                // TERMINAL_LOG=pty (needs --features trace) dumps every raw
                // PTY output chunk with escapes made visible -- the ground
                // truth for "what did the shell actually send", e.g. how fish
                // draws its dim autosuggestion and repositions the cursor.
                tlog::trace!(Pty, "{}", tlog::Escaped(bytes));
                let grid = &mut self.grid;
                self.parser.feed_to(bytes, grid);
            }
            ParserMsg::Resize { cols, rows } => self.grid.resize(cols, rows),
            ParserMsg::Scroll { delta } => self.grid.scroll_viewport(delta),
        }
    }

    /// Push a one-shot bell/title/link/clipboard event to the UI thread
    /// over the SPSC `ui_events_tx` ring, waking it the same way `publish`
    /// wakes it for a dirty snapshot. Drops the event (with a debug log,
    /// not a panic) if the small `RING_UI_EVENTS` ring is momentarily full
    /// -- these are advisory UI niceties (bell/title/link/clipboard), not
    /// grid content, so an extremely rare drop under a pathological burst
    /// is preferable to blocking the parser thread or growing the ring
    /// unbounded.
    fn push_ui_event(&self, ev: UiEvent) {
        if self.ui_events_tx.push(ev).is_err() {
            tlog::trace!(Parser, "ui_events ring full, dropping a UiEvent");
        }
        self.ui_wake.signal();
    }

    /// Push any host-bound replies the grid produced (CPR/DA) to the PTY
    /// thread.
    fn flush_replies(&mut self) {
        let replies = self.grid.take_replies();
        if !replies.is_empty() {
            let _ = self
                .reply_tx
                .push(InputMsg::Bytes(replies.into_boxed_slice()));
            self.pty_wake.signal();
        }
    }

    /// Publish the current dirty snapshot to the UI ring. Never drops a
    /// snapshot: if the ring is momentarily full (the UI thread lagging
    /// under a render-heavy frame), merge the would-be-lost
    /// rows/cursor/scroll-delta back into the grid's own dirty tracker so
    /// the *next* `take_dirty` emits them instead -- the renderer only ever
    /// needs the newest complete picture, and this guarantees no row
    /// silently vanishes.
    fn publish(&mut self) {
        let snap = self.grid.take_dirty();
        if snap.is_empty() {
            return;
        }
        if let Err(unsent) = self.dirty_tx.push(snap) {
            self.grid.requeue_snapshot(&unsent);
        } else {
            self.ui_wake.signal(); // UI polls an eventfd, not this ring
        }
    }

    fn run(mut self) {
        while !self.quit.load(Ordering::Relaxed) {
            // Block for the next message, then drain whatever else is queued
            // so a burst is handled in one batch — but strictly in arrival
            // order, so a Resize and the Data that follows it stay correctly
            // sequenced.
            //
            // The batch is capped (count and wall-clock time) so a flood of
            // PTY output can't keep this thread feeding the grid
            // indefinitely without ever publishing a snapshot: every
            // BATCH_CHUNK_CAP messages or BATCH_TIME_CAP of wall time,
            // whichever comes first, we publish what we have and re-enter
            // the loop. Streaming output then looks smooth under flood
            // instead of stalling then jumping, at no throughput cost (each
            // batch is still up to 64 x 4 KiB = 256 KiB).
            let Some(first) = self.parser_rx.pop_block() else {
                break;
            };
            self.feed(first);
            let batch_start = Instant::now();
            let mut batched = 1usize;
            while batched < BATCH_CHUNK_CAP && batch_start.elapsed() < BATCH_TIME_CAP {
                let Some(msg) = self.parser_rx.pop() else {
                    break;
                };
                self.feed(msg);
                batched += 1;
            }

            // Every drained slot just freed room in the (MPSC) parser_in
            // ring, which is exactly what the pty thread's `on_output`
            // closure is waiting for when it's throttled by real
            // backpressure (its read SQE deliberately left un-armed while
            // that ring was full -- see `Pty::run`). The pty thread only
            // re-checks on an io_uring wakeup (signalfd or this wake
            // eventfd), so it must be signalled here or a throttled read
            // could sit un-retried until an unrelated signal happens to
            // arrive. Cheap (one eventfd write) even when nothing was
            // actually throttled.
            self.pty_wake.signal();

            self.flush_replies();

            // Publish bracketed-paste mode for the UI thread (it wraps
            // clipboard paste in the 200~/201~ markers when the app asked
            // for it).
            self.bracketed_paste
                .store(self.grid.bracketed_paste(), Ordering::Relaxed);

            // Publish the rest of the negotiated terminal modes (mouse
            // tracking/SGR, keyboard protocol, app-cursor/keypad, focus
            // events) for the UI thread's `Encoder`.
            self.term_modes
                .store(TermModes::from_grid(&self.grid).pack(), Ordering::Relaxed);

            // Queue one-shot bell/title events for the UI thread over the
            // SPSC ui_events ring. Unlike the level-triggered state above,
            // these must be observed exactly once each, so they're queued
            // rather than overwritten.
            if self.grid.take_bell() {
                self.push_ui_event(UiEvent::Bell);
            }
            if let Some(title) = self.grid.take_title() {
                self.push_ui_event(UiEvent::Title(title));
            }
            for (id, uri) in self.grid.take_new_links() {
                self.push_ui_event(UiEvent::Link(id, uri));
            }
            if let Some(text) = self.grid.take_clipboard() {
                self.push_ui_event(UiEvent::Clipboard(text));
            }
            for idx in self.grid.take_palette_changes() {
                self.push_ui_event(UiEvent::PaletteColor(idx, self.grid.palette_color(idx)));
            }

            self.publish();
        }
    }
}

// ── UI thread (main thread) ───────────────────────────────────────────────────

/// Owns every ring endpoint the UI (main) thread touches, plus the config
/// and shared flags it reads. Was an eight-argument free function
/// (`ui_thread`); the two cross-thread translation helpers it called
/// (`platform_mods`, `platform_key`) stay free -- they're pure conversions
/// between two sibling crates' types, not operations on this worker's state.
struct UiWorker<'a> {
    cfg: &'a Config,
    dirty_rx: quetzalcoatl::spsc::Consumer<DirtySnapshot>,
    input_tx: mpsc::Producer<InputMsg>,
    resize_tx: mpsc::Producer<ParserMsg>,
    scroll_tx: mpsc::Producer<ParserMsg>,
    ui_wake: platform::Waker,
    pty_wake: waker::Waker,
    bracketed_paste: &'a AtomicBool,
    term_modes: &'a AtomicU32,
    ui_events_rx: quetzalcoatl::spsc::Consumer<UiEvent>,
    quit: &'a AtomicBool,
    /// See `App::shell_pid`. Read on Ctrl+Shift+Enter to resolve where to
    /// open the new terminal window.
    shell_pid: &'a AtomicI32,
}

/// Owns every piece of mutable state one iteration of the UI event loop
/// touches -- the Wayland window, the VT input encoder, resize/link
/// bookkeeping -- plus the channel endpoints and shared flags `UiWorker`
/// was built with. Splitting this out of `UiWorker` (which stays the plain
/// "here are your channels" argument bundle `App::run` constructs) is what
/// lets each `InputEvent` variant get its own small, independently-readable
/// handler method instead of one 400-line match arm.
struct UiLoop<'a> {
    dirty_rx: quetzalcoatl::spsc::Consumer<DirtySnapshot>,
    input_tx: mpsc::Producer<InputMsg>,
    resize_tx: mpsc::Producer<ParserMsg>,
    scroll_tx: mpsc::Producer<ParserMsg>,
    pty_wake: waker::Waker,
    bracketed_paste: &'a AtomicBool,
    term_modes: &'a AtomicU32,
    ui_events_rx: quetzalcoatl::spsc::Consumer<UiEvent>,
    quit: &'a AtomicBool,
    /// See `App::shell_pid`. Read on Ctrl+Shift+Enter to resolve where to
    /// open the new terminal window.
    shell_pid: &'a AtomicI32,

    win: TermWindow,
    encoder: Encoder,
    /// Idle poll cap while an animation (cursor slide/blink) is in flight, in
    /// milliseconds -- derived from `[window] target_fps`. This is a *target*
    /// poll cadence, not a guaranteed frame rate: the actual ceiling is the
    /// compositor's own vsync (`wl_callback.done` gates every present in
    /// `TermWindow::maybe_present`), so on a 60Hz output setting this above 60
    /// just means the loop wakes more often than it can usefully present,
    /// spending a little extra CPU for lower input-to-animation latency. On a
    /// 90/120/144Hz+ output it lets the animation actually track the higher
    /// refresh rate. `target_fps` is validated to `1..=1000` by config, so
    /// this is always `>= 1`.
    anim_poll_cap_ms: i32,
    /// The grid's current row count for page-scroll sizing (updated on
    /// resize). Seeded from the startup dimensions.
    grid_rows: u16,
    /// Holds back the PTY SIGWINCH during a drag so the shell reprompts once
    /// at the settled size instead of at every intermediate width (which
    /// strands truncated prompt fragments). The grid still reflows live.
    resize_debounce: ResizeDebounce,
    /// Mirror of the grid's OSC 8 link table, id -> URI, built incrementally
    /// from `UiEvent::Link`. Lets a Ctrl+click resolve the cell under the
    /// pointer's `link_id` without reaching across to the parser thread.
    links: std::collections::HashMap<u32, String>,
    /// Says which way a Ctrl+click's token request degraded, when
    /// `COZY_DEBUG_ACTIVATION` is set. Read once here rather than per click.
    activation_report: platform::toplevel::activation_report::ActivationReport,
    /// See `ConfigWatcher`'s doc for the save-detection strategy and why
    /// it's best-effort.
    config_watcher: ConfigWatcher,
}

impl<'a> UiLoop<'a> {
    /// Build the window, encoder, and loop bookkeeping from a `UiWorker`'s
    /// channels/config. Split out of `run` purely so construction (setting
    /// up the theme/cursor/window) doesn't compete for attention with the
    /// loop body below.
    fn new(worker: UiWorker<'a>) -> platform::Result<Self> {
        let UiWorker {
            cfg,
            dirty_rx,
            input_tx,
            resize_tx,
            scroll_tx,
            ui_wake,
            pty_wake,
            bracketed_paste,
            term_modes,
            ui_events_rx,
            quit,
            shell_pid,
        } = worker;
        let theme = platform::Theme::from_palette(&cfg.palette);
        let font = cfg.font.clone();
        let opacity = cfg.window.opacity;
        // Map the cursor config to the platform's animation config. blink_rate is
        // the half-period (ms per on/off); the animator wants the full cycle.
        let cursor_cfg = platform::CursorAnimConfig {
            shape: match cfg.cursor.style {
                config::CursorStyle::Block => grid::CursorShape::Block,
                config::CursorStyle::Beam => grid::CursorShape::Bar,
                config::CursorStyle::Underline => grid::CursorShape::Underline,
            },
            blink: cfg.cursor.blink,
            // `blink_rate` is config-validated to a small millisecond range
            // (see config::CursorConfig), nowhere near f32's 24-bit exact
            // integer range.
            #[allow(clippy::cast_precision_loss)]
            period_ms: (cfg.cursor.blink_rate * 2) as f32,
        };
        // TermWindow reads the output scale and calls this closure to rasterize
        // the font at native density (HiDPI-aware).
        let win = TermWindow::new("terminal", COLS, ROWS, ui_wake, cursor_cfg, move |scale| {
            platform::Renderer::with_scale(theme, &font, scale).with_opacity(opacity)
        })?;
        let anim_poll_cap_ms = (1000 / cfg.window.target_fps.max(1)).max(1).cast_signed();
        Ok(Self {
            dirty_rx,
            input_tx,
            resize_tx,
            scroll_tx,
            pty_wake,
            bracketed_paste,
            term_modes,
            ui_events_rx,
            quit,
            shell_pid,
            win,
            encoder: Encoder::new(),
            anim_poll_cap_ms,
            grid_rows: ROWS,
            resize_debounce: ResizeDebounce::new(),
            links: std::collections::HashMap::new(),
            activation_report:
                platform::toplevel::activation_report::ActivationReport::from_env("terminal"),
            config_watcher: ConfigWatcher::new(),
        })
    }

    /// Run the event loop until quit.
    fn run(mut self) -> platform::Result<()> {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }

            let mut did_work = false;
            // True if we pushed input the PTY thread must be roused for.
            let mut sent_input = false;

            self.apply_negotiated_modes();
            self.drain_ui_events(&mut did_work);
            self.drain_dirty_snapshots(&mut did_work);
            // `Close` bypasses the normal end-of-iteration batching below (the
            // PTY is being told to hang up right now), so a `true` return
            // means "stop the whole loop and return `Ok(())` immediately".
            if self.drain_input_events(&mut did_work, &mut sent_input) {
                return Ok(());
            }
            self.flush_debounced_resize(&mut sent_input);

            // Rouse the PTY thread once for the whole batch of queued input.
            if sent_input {
                self.pty_wake.signal();
            }

            // Config live-reload: see `ConfigWatcher::poll_reload`.
            if self.config_watcher.poll_reload(&mut self.win) {
                did_work = true;
            }

            if !self.win.tick()? {
                break;
            }

            if !did_work {
                self.idle_wait();
            }
        }

        self.quit.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Pick up whatever mode changes the parser thread negotiated since the
    /// last iteration (mouse tracking/SGR, keyboard protocol, app-cursor/
    /// keypad, focus events) before encoding any input this iteration
    /// produces, and let the window know whether the app owns the pointer
    /// (mouse tracking negotiated): gates the terminal-side selection so
    /// clicking in vim doesn't paint a stray highlight over its UI.
    fn apply_negotiated_modes(&mut self) {
        let negotiated = TermModes::unpack(self.term_modes.load(Ordering::Relaxed));
        negotiated.apply_to(&mut self.encoder);
        self.win
            .set_app_mouse(negotiated.mouse_mode != MouseMode::Off);
    }

    /// Drain one-shot bell/title/link/clipboard/palette events the parser
    /// thread queued over the SPSC `ui_events_rx` ring.
    fn drain_ui_events(&mut self, did_work: &mut bool) {
        while let Some(ev) = self.ui_events_rx.pop() {
            match ev {
                UiEvent::Bell => self.win.bell(),
                UiEvent::Title(title) => self.win.set_title(&title),
                UiEvent::Link(id, uri) => {
                    self.links.insert(id, uri);
                }
                UiEvent::Clipboard(text) => self.win.write_clipboard(text),
                UiEvent::PaletteColor(idx, rgb) => self.win.apply_palette_change(idx, rgb),
            }
            *did_work = true;
        }
    }

    /// Push every pending dirty snapshot into the Wayland renderer.
    fn drain_dirty_snapshots(&mut self, did_work: &mut bool) {
        while let Some(snap) = self.dirty_rx.pop() {
            self.win.push_dirty(snap);
            *did_work = true;
        }
    }

    /// Drain and dispatch every queued window input event this iteration.
    /// Returns `true` when the caller should return `Ok(())` from `run`
    /// right now -- only `InputEvent::Close` does this.
    fn drain_input_events(&mut self, did_work: &mut bool, sent_input: &mut bool) -> bool {
        while let Some(ev) = self.win.next_input() {
            *did_work = true;
            match ev {
                InputEvent::Close => {
                    self.quit.store(true, Ordering::Relaxed);
                    let _ = self.input_tx.push(InputMsg::Quit);
                    self.pty_wake.signal();
                    return true;
                }
                InputEvent::Resize { cols, rows } => self.handle_resize(cols, rows),
                InputEvent::Scroll {
                    lines,
                    col,
                    row,
                    modifiers,
                } => {
                    if self.handle_scroll(lines, col, row, modifiers) {
                        *sent_input = true;
                    }
                }
                InputEvent::Paste(bytes) => {
                    if self.handle_paste(bytes) {
                        *sent_input = true;
                    }
                }
                InputEvent::Key {
                    key,
                    state,
                    modifiers,
                } => {
                    if self.handle_key_event(key, state, modifiers) {
                        *sent_input = true;
                    }
                }
                InputEvent::Mouse {
                    button,
                    kind,
                    col,
                    row,
                    modifiers,
                } => {
                    if self.handle_mouse_event(button, kind, col, row, modifiers) {
                        *sent_input = true;
                    }
                }
                InputEvent::Focus(gained) => {
                    if self.handle_focus_event(gained) {
                        *sent_input = true;
                    }
                }
            }
        }
        false
    }

    /// `InputEvent::Resize`: reflow the grid NOW (live visual feedback),
    /// pushing to the parser MPSC ring which unparks the parser directly --
    /// no separate wake needed. But DEFER the PTY winsize: sending SIGWINCH
    /// on every drag step makes the shell reprompt at each intermediate
    /// width, stranding truncated prompt fragments. `ResizeDebounce` releases
    /// one winsize once the drag settles.
    fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.grid_rows = rows;
        let _ = self.resize_tx.push(ParserMsg::Resize { cols, rows });
        self.resize_debounce.note(cols, rows);
    }

    /// `InputEvent::Scroll`: route the wheel by what the app negotiated
    /// (kitty/foot semantics; Shift bypasses to the terminal's own
    /// scrollback in every case):
    ///  1. app mouse tracking → encode wheel-button press reports (vim's
    ///     `mouse=a` scrolls its buffer);
    ///  2. alt screen, no tracking → "alternate scroll": arrow keys per line
    ///     (`less`, pagers);
    ///  3. otherwise → scroll the grid viewport into scrollback (the shell
    ///     prompt case).
    ///
    /// Returns whether any bytes were queued for the PTY.
    fn handle_scroll(
        &self,
        lines: i32,
        col: u16,
        row: u16,
        modifiers: platform::protocol::Modifiers,
    ) -> bool {
        let mods = modifiers.to_input();
        let negotiated = TermModes::unpack(self.term_modes.load(Ordering::Relaxed));
        let mut sent = false;
        if negotiated.mouse_mode != MouseMode::Off && !mods.shift() {
            let button = if lines > 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            let mev = MouseEvent {
                button,
                state: input::mouse::ButtonState::Pressed,
                col,
                row,
                mods,
            };
            let mut mbuf = [0u8; 32];
            for _ in 0..lines.unsigned_abs() {
                if let Some(n) = self.encoder.encode_mouse(mev, &mut mbuf) {
                    let _ = self.input_tx.push(InputMsg::Bytes(mbuf[..n].into()));
                    sent = true;
                }
            }
        } else if negotiated.alt_screen && !mods.shift() {
            // Alternate scroll: Up/Down arrows, one per line,
            // honoring DECCKM (SS3 vs CSI) via the encoder.
            let key = if lines > 0 { Key::Up } else { Key::Down };
            let mut kbuf = [0u8; 16];
            for _ in 0..lines.unsigned_abs() {
                if let Some(n) = self.encoder.encode_key(
                    key,
                    KeyState::Pressed,
                    input::Modifiers::default(),
                    &mut kbuf,
                ) {
                    let _ = self.input_tx.push(InputMsg::Bytes(kbuf[..n].into()));
                    sent = true;
                }
            }
        } else {
            let _ = self.scroll_tx.push(ParserMsg::Scroll { delta: lines });
        }
        sent
    }

    /// `InputEvent::Paste`: clipboard contents from another client, written
    /// to the PTY so the shell/app receives them as input. When the app
    /// enabled bracketed paste (DECSET 2004), wrap in the 200~/201~ markers
    /// so pasted newlines are treated as text, not as typed Enter (fish/zsh/
    /// neovim rely on this). Always sends, so always returns `true`.
    fn handle_paste(&self, bytes: Vec<u8>) -> bool {
        let data = if self.bracketed_paste.load(Ordering::Relaxed) {
            let mut wrapped = Vec::with_capacity(bytes.len() + 12);
            wrapped.extend_from_slice(b"\x1b[200~");
            wrapped.extend_from_slice(&bytes);
            wrapped.extend_from_slice(b"\x1b[201~");
            wrapped
        } else {
            bytes
        };
        let _ = self.input_tx.push(InputMsg::Bytes(data.into_boxed_slice()));
        true
    }

    /// `InputEvent::Key`: resolves a released/unmapped key to nothing,
    /// handles the terminal's own Shift+PgUp/PgDn scrollback paging and
    /// Ctrl+Shift+C/V/Enter clipboard/sibling-window shortcuts (never sent
    /// to the shell), and otherwise encodes + queues the key for the PTY.
    /// Returns whether any bytes were queued.
    fn handle_key_event(
        &mut self,
        key: Option<platform::protocol::Key>,
        state: Option<platform::protocol::KeyState>,
        modifiers: platform::protocol::Modifiers,
    ) -> bool {
        use platform::protocol::KeyState as PKeyState;
        let Some(kstate) = (match state {
            Some(PKeyState::Pressed) => Some(KeyState::Pressed),
            Some(PKeyState::Released) => Some(KeyState::Released),
            None => None,
        }) else {
            return false;
        };
        if kstate == KeyState::Released && self.encoder.keyboard_mode == input::KeyboardMode::Legacy
        {
            return false;
        }
        let mods = modifiers.to_input();
        let Some(ikey) = key.to_input() else {
            return false;
        };
        if self.try_shift_page_scroll(kstate, mods, ikey)
            || self.try_clipboard_shortcut(kstate, mods, ikey)
        {
            return false;
        }
        let mut ibuf = [0u8; 64];
        if let Some(n) = self.encoder.encode_key(ikey, kstate, mods, &mut ibuf) {
            let _ = self.input_tx.push(InputMsg::Bytes(ibuf[..n].into()));
            // Any real keystroke snaps the cursor blink back to
            // full opacity -- it should never be caught mid-fade
            // by what you just typed.
            self.win.notify_activity();
            return true;
        }
        false
    }

    /// Shift+PgUp/PgDn scroll the grid viewport by a page instead of
    /// sending the escape to the shell (bare `PgUp`/`PgDn` still go to the
    /// shell for history search). Split out of `handle_key_event` -- this
    /// and [`Self::try_clipboard_shortcut`] are two independent
    /// "intercept this key combo before it reaches the shell" checks that
    /// were each contributing their own match arms to one function's
    /// complexity. Returns whether the combo was recognised (and thus
    /// already fully handled).
    fn try_shift_page_scroll(&self, kstate: KeyState, mods: input::Modifiers, ikey: Key) -> bool {
        if kstate != KeyState::Pressed || !mods.shift() {
            return false;
        }
        let page = i32::from(self.grid_rows);
        match ikey {
            Key::PageUp => {
                let _ = self.scroll_tx.push(ParserMsg::Scroll { delta: page });
                true
            }
            Key::PageDown => {
                let _ = self.scroll_tx.push(ParserMsg::Scroll { delta: -page });
                true
            }
            _ => false,
        }
    }

    /// Ctrl+Shift+C / Ctrl+Shift+V / Ctrl+Shift+Enter: copy / paste via the
    /// system clipboard, or spawn a sibling shell in the same directory --
    /// all handled by the window, never sent to the shell. Split out of
    /// `handle_key_event` for the same reason as
    /// [`Self::try_shift_page_scroll`]. Returns whether the combo was
    /// recognised.
    fn try_clipboard_shortcut(
        &mut self,
        kstate: KeyState,
        mods: input::Modifiers,
        ikey: Key,
    ) -> bool {
        if kstate != KeyState::Pressed || !mods.ctrl() || !mods.shift() {
            return false;
        }
        match ikey {
            Key::Char('c') => {
                self.win.copy_selection();
                true
            }
            Key::Char('v') => {
                self.win.request_paste();
                true
            }
            Key::Enter => {
                SiblingSpawner::open_here(self.shell_pid.load(Ordering::Relaxed));
                true
            }
            _ => false,
        }
    }

    /// `InputEvent::Mouse`: Ctrl+click on an OSC 8 hyperlink opens it in the
    /// user's default handler instead of being forwarded to the shell --
    /// matching every other terminal's hyperlink convention (kitty, foot,
    /// `WezTerm`, ...). Only fires when no app has claimed mouse reporting
    /// (`mouse_mode == Off`): a mouse-aware app like vim gets the click as
    /// usual and can handle OSC 8 itself. Otherwise encodes + queues a VT
    /// mouse report. Returns whether any bytes were queued.
    fn handle_mouse_event(
        &mut self,
        button: platform::window::MouseButton,
        kind: platform::window::MouseEventKind,
        col: u16,
        row: u16,
        modifiers: platform::protocol::Modifiers,
    ) -> bool {
        use platform::window::MouseEventKind;
        if self.try_open_hyperlink(button, kind, col, row, modifiers) {
            return false;
        }
        let mev = MouseEvent {
            button: match button {
                platform::window::MouseButton::Left => MouseButton::Left,
                platform::window::MouseButton::Middle => MouseButton::Middle,
                platform::window::MouseButton::Right => MouseButton::Right,
                platform::window::MouseButton::WheelUp => MouseButton::WheelUp,
                platform::window::MouseButton::WheelDown => MouseButton::WheelDown,
                platform::window::MouseButton::None => MouseButton::None,
            },
            state: match kind {
                MouseEventKind::Press => input::mouse::ButtonState::Pressed,
                MouseEventKind::Release => input::mouse::ButtonState::Released,
                MouseEventKind::Motion => input::mouse::ButtonState::Moved,
            },
            col,
            row,
            mods: modifiers.to_input(),
        };
        let mut mbuf = [0u8; 32];
        if let Some(n) = self.encoder.encode_mouse(mev, &mut mbuf) {
            let _ = self.input_tx.push(InputMsg::Bytes(mbuf[..n].into()));
            return true;
        }
        false
    }

    /// Ctrl+click on an OSC 8 hyperlink: open it in the user's default
    /// handler instead of forwarding to the shell -- matching every other
    /// terminal's hyperlink convention (kitty, foot, `WezTerm`, ...). Also
    /// fires on a plain-text URL with no OSC 8 escape at all -- e.g. `trunk
    /// serve`'s "listening at http://..." banner -- via
    /// [`platform::link_scan::UrlScanner`], since plenty of real-world tools
    /// never emit the escape in the first place. Only fires when no app has
    /// claimed mouse reporting (`mouse_mode == Off`): a mouse-aware app like
    /// vim gets the click as usual and can handle OSC 8 itself. Split out of
    /// `handle_mouse_event` -- this whole precondition-laden branch was one
    /// self-contained "is this a hyperlink click" concern folded into a
    /// function whose real job is
    /// building + encoding a VT mouse report. Returns whether a link was
    /// opened (and the click should NOT also be forwarded to the shell).
    fn try_open_hyperlink(
        &mut self,
        button: platform::window::MouseButton,
        kind: platform::window::MouseEventKind,
        col: u16,
        row: u16,
        modifiers: platform::protocol::Modifiers,
    ) -> bool {
        use platform::window::MouseEventKind;
        if kind != MouseEventKind::Press
            || button != platform::window::MouseButton::Left
            || !modifiers.to_input().ctrl()
            || TermModes::unpack(self.term_modes.load(Ordering::Relaxed)).mouse_mode
                != MouseMode::Off
        {
            return false;
        }
        // Real OSC 8 links resolve through our `link_id -> uri` mirror;
        // plain-text URLs (no application-side escape at all -- e.g. `trunk
        // serve`'s banner) carry their own URI straight from the scanner.
        // See `platform::link_scan` for why both exist.
        let uri = match self.win.link_target_at(col, row) {
            Some(platform::link_scan::LinkTarget::Real(id)) => {
                let Some(uri) = self.links.get(&id) else {
                    return false;
                };
                uri.clone()
            }
            Some(platform::link_scan::LinkTarget::Url(span)) => span.uri,
            None => return false,
        };
        self.open_link(&uri);
        true
    }

    /// Hand a clicked link to the application associated with it.
    ///
    /// The databases are loaded per click rather than held: opening a link
    /// is a rare, human-paced action, and a terminal that cached them at
    /// startup would keep answering with the associations that existed then.
    ///
    /// The token is minted here, inside the click handler, because it must
    /// carry *this* click's serial. Minting it earlier or later would present
    /// the compositor evidence of a different moment.
    fn open_link(&mut self, uri: &str) {
        let token = self.win.activation_token(None);
        self.activation_report
            .record(token.as_ref(), self.win.supports_activation());
        let opener = opener::Opener::load()
            .with_activation(token.map(|t| opener::ActivationToken::new(t.into_string())));
        if let Err(error) = opener.try_open(&opener::Target::parse(uri)) {
            eprintln!("terminal: {uri}: {error}");
        }
    }

    /// `InputEvent::Focus`: DECSET 1004 focus in/out reporting (`CSI I` /
    /// `CSI O`), gated on the negotiated flag so a plain shell (which never
    /// asked for this) doesn't see stray escapes land on its prompt. Returns
    /// whether the escape was queued.
    fn handle_focus_event(&self, gained: bool) -> bool {
        if TermModes::unpack(self.term_modes.load(Ordering::Relaxed)).focus_events {
            let bytes: &[u8] = if gained { b"\x1b[I" } else { b"\x1b[O" };
            let _ = self.input_tx.push(InputMsg::Bytes(bytes.into()));
            return true;
        }
        false
    }

    /// Release a debounced winsize once the drag has settled: send ONE
    /// SIGWINCH at the final size so the shell reprompts exactly once.
    fn flush_debounced_resize(&mut self, sent_input: &mut bool) {
        if let Some((cols, rows)) = self.resize_debounce.take_if_settled() {
            let _ = self.input_tx.push(InputMsg::Resize { cols, rows });
            *sent_input = true;
        }
    }

    /// Idle: block in poll(2) on the UI wakeup eventfd instead of
    /// busy-spinning. The Wayland reader signals it after pushing protocol
    /// events; the parser signals it after producing a dirty snapshot.
    /// `win.wait` drains the counter after waking; any signal that raced in
    /// during this iteration's reads leaves the counter set, so the poll
    /// returns at once (no lost wakeup).
    ///
    /// While the cursor is mid-slide or blinking-and-visible, cap the wait at
    /// `anim_poll_cap_ms` instead of the normal 100ms so the animation
    /// actually advances smoothly instead of updating in big 100ms jumps;
    /// otherwise it stays the full 100ms, bounding only quit-flag latency as
    /// before. A pending debounced resize must also wake the loop when its
    /// quiet period elapses, even if nothing else arrives — otherwise the
    /// held winsize could sit unsent until the next unrelated event.
    fn idle_wait(&self) {
        let mut cap = if self.win.wants_animation_frame() {
            self.anim_poll_cap_ms
        } else {
            100
        };
        if let Some(resize_cap) = self.resize_debounce.wait_cap_ms() {
            cap = cap.min(resize_cap.max(1));
        }
        self.win.wait(cap);
    }
}

// ── Resize debounce ────────────────────────────────────────────────────────────────────

/// Debounces the SIGWINCH sent to the PTY during a window drag.
///
/// A drag emits a flood of cell-count changes. The *grid* is reflowed on
/// every one of them (immediate, so the visible text tracks the window
/// crisply), but the *PTY* winsize must NOT be — every winsize raises
/// SIGWINCH, and the shell answers each by repainting its prompt. At
/// intermediate widths those repaints land on a grid that the very next
/// reflow step immediately re-lays-out, stranding the half-drawn prompt as a
/// truncated fragment (`~/C/f/te`, `~/C/f/term`, …) one row above the live
/// one — and a fast drag piles dozens of them up.
///
/// So we hold back the winsize: record the latest (cols, rows) and a
/// deadline a short quiet-period into the future, pushing it forward on each
/// new step. Only once the drag goes quiet for `QUIET` do we send ONE winsize
/// at the settled size, so the shell reprompts exactly once, cleanly. The
/// terminal already sizes the kernel PTY correctly at startup, and any app
/// that truly cares about live size (a full-screen TUI) re-queries on its own
/// SIGWINCH once the drag stops — the debounce only delays the signal by a
/// few frames, never drops it.
struct ResizeDebounce {
    /// The latest proposed size, plus the instant we may release it. `None`
    /// when no resize is pending (steady state).
    pending: Option<(u16, u16, Instant)>,
}

impl ResizeDebounce {
    /// Quiet period a drag must be idle before the held winsize is flushed to
    /// the PTY. ~80ms is below perceptible lag on drag-release yet long
    /// enough to swallow a whole drag's configure storm (which arrives at
    /// screen-refresh cadence, ≤16ms apart).
    const QUIET: Duration = Duration::from_millis(80);

    /// No resize pending.
    const fn new() -> Self {
        Self { pending: None }
    }

    /// Record a new proposed size, (re)arming the quiet-period deadline. The
    /// grid is reflowed by the caller regardless; only the PTY winsize is
    /// what this gate defers.
    fn note(&mut self, cols: u16, rows: u16) {
        self.pending = Some((cols, rows, Instant::now() + Self::QUIET));
    }

    /// If a size is pending and its quiet period has elapsed, consume and
    /// return it (the caller then sends the single winsize). Returns `None`
    /// while a drag is still in flight or nothing is pending.
    fn take_if_settled(&mut self) -> Option<(u16, u16)> {
        match self.pending {
            Some((cols, rows, deadline)) if Instant::now() >= deadline => {
                self.pending = None;
                Some((cols, rows))
            }
            _ => None,
        }
    }

    /// Milliseconds until the pending winsize should be flushed, if any — so
    /// the event loop can cap its idle `poll` wait and wake to release it
    /// even when no other event arrives. `None` when nothing is pending.
    fn wait_cap_ms(&self) -> Option<i32> {
        self.pending.map(|(_, _, deadline)| {
            let now = Instant::now();
            if now >= deadline {
                0
            } else {
                // The remaining window is at most QUIET (~80ms), far inside
                // i32 range; saturate defensively anyway.
                i32::try_from((deadline - now).as_millis()).unwrap_or(i32::MAX)
            }
        })
    }
}

// ── Config live-reload ────────────────────────────────────────────────────────────────────

/// Watches the config file for saves and hot-applies whatever's safe to
/// change without a restart. Was two free functions (`inotify_touched`,
/// `apply_live_config`) plus three loose locals (`config_path`,
/// `config_watch`, `config_file_name`) threaded through `UiLoop::run`'s
/// loop body -- bundled here since they're one cohesive unit: the watch
/// handle, the filename it's filtering for, and the reload/apply logic
/// that consumes them are never meaningful apart from each other.
struct ConfigWatcher {
    /// `None` if the config directory doesn't exist yet or inotify setup
    /// failed -- live-reload is then simply unavailable, not an error (see
    /// `Self::new`).
    inotify: Option<ququmatz::Inotify>,
    /// The config file's bare name (no directory), to filter the
    /// directory-wide watch down to saves of *this* file specifically.
    file_name: Option<String>,
    path: std::path::PathBuf,
}

impl ConfigWatcher {
    /// Set up the watch, best-effort: if the config dir doesn't exist yet
    /// (nothing has ever been saved there) or inotify setup fails,
    /// live-reload is simply unavailable and the rest of the terminal works
    /// exactly as before.
    ///
    /// Watches the config file for writes (most editors save via
    /// rename-into-place, which is `IN_MOVE_SELF`/`IN_ATTRIB` on the old
    /// inode or `IN_CREATE` in the directory depending on the editor --
    /// watching the *directory* for `CLOSE_WRITE|MOVED_TO` on any entry and
    /// filtering by filename catches every common save strategy, where
    /// watching the file itself would miss an atomic rename replacing the
    /// inode out from under the watch).
    fn new() -> Self {
        let path = config::Config::xdg_config_path();
        let inotify = path.parent().and_then(|dir| {
            let ino = ququmatz::Inotify::new().ok()?;
            let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
            ino.add_watch(
                &dir_c,
                ququmatz::WatchMask::CLOSE_WRITE | ququmatz::WatchMask::MOVED_TO,
            )
            .ok()?;
            Some(ino)
        });
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        Self {
            inotify,
            file_name,
            path,
        }
    }

    /// If our config file was touched since the last call, re-parse and
    /// hot-apply it to `win`. Returns whether anything happened (so the
    /// caller can fold it into its own `did_work` bookkeeping). `[general]`
    /// (shell, scrollback size, ...) and `[keybinds]` still need a restart
    /// -- they're consumed once at process startup on other threads this
    /// has no channel to.
    fn poll_reload(&self, win: &mut TermWindow) -> bool {
        let (Some(ino), Some(fname)) = (&self.inotify, &self.file_name) else {
            return false;
        };
        if !Self::touched(ino, fname) {
            return false;
        }
        match config::Config::load_from(&self.path) {
            Ok(new_cfg) => {
                Self::apply(&new_cfg, win);
                true
            }
            Err(e) => {
                eprintln!("terminal: config reload failed, keeping old config: {e}");
                false
            }
        }
    }

    /// Drain every pending inotify event on the config directory's watch and
    /// report whether any of them named `fname` (our config file
    /// specifically, not some unrelated file saved alongside it in the same
    /// directory). Non-blocking -- the fd was opened with `IN_NONBLOCK`, so
    /// an empty read just means "nothing changed", not a stall.
    fn touched(ino: &ququmatz::Inotify, fname: &str) -> bool {
        inotify_events::InotifyEvents::any_touched(ino, fname)
    }

    /// Hot-apply the subset of a freshly-reloaded config that's safe to
    /// change without restarting the process: theme/palette, font, window
    /// opacity, and cursor style/blink. Everything else (`[general]`'s
    /// shell/scrollback, `[keybinds]`) is read once at startup on threads
    /// this has no channel to reach, so it's simply left as-is until the
    /// next restart -- consistent with how most terminals scope
    /// live-reload to appearance, not process-lifetime settings.
    fn apply(cfg: &Config, win: &mut TermWindow) {
        let theme = platform::Theme::from_palette(&cfg.palette);
        let renderer = platform::Renderer::with_scale(theme, &cfg.font, win.output_scale())
            .with_opacity(cfg.window.opacity);
        if win.needs_alpha_changed(&renderer) {
            eprintln!(
                "terminal: config reload: [window] opacity crossed the opaque/translucent boundary -- restart to apply (buffer format is fixed at startup)"
            );
            return;
        }
        win.set_renderer(renderer);
        win.set_cursor_config(platform::CursorAnimConfig {
            shape: match cfg.cursor.style {
                config::CursorStyle::Block => grid::CursorShape::Block,
                config::CursorStyle::Beam => grid::CursorShape::Bar,
                config::CursorStyle::Underline => grid::CursorShape::Underline,
            },
            blink: cfg.cursor.blink,
            #[allow(clippy::cast_precision_loss)]
            period_ms: (cfg.cursor.blink_rate * 2) as f32,
        });
    }
}

// ── Ctrl+Shift+Enter: open a new terminal in the same CWD ─────────────────────

/// Extends `i32` (a process id) with "walk to the deepest living child" as
/// a method, instead of a free `deepest_child(pid)` function -- per the
/// crate-wide rule, logic with no natural struct owner belongs on a trait
/// over the primitive it operates on.
trait ChildTree {
    /// Walk down from this pid to its deepest living child (the PTY's
    /// foreground job -- usually the shell itself, or whatever command is
    /// currently running in it), following
    /// `/proc/<pid>/task/<pid>/children`.
    fn deepest_child(self) -> i32;
}

impl ChildTree for i32 {
    fn deepest_child(self) -> Self {
        let mut cur = self;
        while let Ok(raw) = std::fs::read_to_string(format!("/proc/{cur}/task/{cur}/children")) {
            let Some(last) = raw.split_ascii_whitespace().next_back() else {
                break;
            };
            let Ok(next) = last.parse::<Self>() else {
                break;
            };
            cur = next;
        }
        cur
    }
}

/// Marker struct owning the Ctrl+Shift+Enter "open a new terminal here"
/// action, instead of a free `open_terminal_here(shell_pid)` function --
/// there's no natural instance data (it's a one-shot side effect keyed off
/// an argument, not a stateful owner), but it's not shared/reusable logic
/// belonging on some primitive's trait either, so a named marker type is
/// the better fit than a loose free fn, matching `SurfaceBootstrap`/
/// `Base64` elsewhere in this workspace.
struct SiblingSpawner;

impl SiblingSpawner {
    /// Spawn a new instance of this terminal, `cd`'d into wherever the
    /// current one's shell (or its foreground job) is sitting -- matching
    /// the "open a new window in the same directory" convention most
    /// terminals bind to this chord. `shell_pid` is `0` if the PTY thread
    /// hasn't finished forking yet (a vanishingly small startup window); in
    /// that case this is a harmless no-op.
    ///
    /// Implemented terminal-side rather than by a window-manager keybind
    /// because we already own the one piece of state this needs -- the
    /// PTY's child PID -- with no IPC or
    /// `/proc`-scraping-some-other-window's-app-id required.
    fn open_here(shell_pid: i32) {
        if shell_pid <= 0 {
            return;
        }
        let target_pid = shell_pid.deepest_child();
        let cwd = std::fs::read_link(format!("/proc/{target_pid}/cwd")).ok();
        let exe = std::env::current_exe().unwrap_or_else(|_| "mera-terminal".into());
        let mut cmd = std::process::Command::new(exe);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        // Same zombie-avoidance as the OSC 8 Ctrl+click spawn: reap on a
        // throwaway thread instead of leaking a `Child` or blocking the UI
        // loop.
        if let Ok(mut child) = cmd.spawn() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
mod live_reload_tests {
    use super::ConfigWatcher;

    /// Write to a file inside a fresh temp dir, watched the same way
    /// `UiLoop::run` watches the real config directory (`CLOSE_WRITE |
    /// MOVED_TO` on the directory, filtered by filename), and confirm
    /// `ConfigWatcher::touched` reports the save. Exercises the real kernel
    /// ABI -- event header size/layout, name padding/NUL-trimming -- not a
    /// mock.
    #[test]
    fn detects_a_save_to_the_watched_file() {
        let dir =
            std::env::temp_dir().join(format!("terminal-live-reload-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("config.toml");
        std::fs::write(&file, "initial").unwrap();

        let ino = ququmatz::Inotify::new().expect("inotify_init1");
        let dir_c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
        ino.add_watch(
            &dir_c,
            ququmatz::WatchMask::CLOSE_WRITE | ququmatz::WatchMask::MOVED_TO,
        )
        .expect("add_watch");

        // No event yet -- a fresh watch with nothing written since must report false.
        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        // A write + close (what every editor's "save" does, atomic-rename
        // editors aside) fires CLOSE_WRITE.
        std::fs::write(&file, "updated").unwrap();
        // inotify delivery is asynchronous; give the kernel a moment.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(ConfigWatcher::touched(&ino, "config.toml"));

        // Already drained -- a second immediate check sees nothing new.
        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        // A save to some other file in the same directory must not be
        // reported as touching our config file.
        std::fs::write(dir.join("unrelated.txt"), "noise").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!ConfigWatcher::touched(&ino, "config.toml"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ── Key / modifier translation ────────────────────────────────────────────────
//
// `platform::protocol::{Key, Modifiers}` and `input::{Key, Modifiers}` are both
// foreign to this crate, so a `From` impl is blocked by the orphan rule --
// these traits are the local-trait workaround, giving the conversions a
// method-call spelling (`raw_mods.to_input()`, `raw_key.to_input()`) instead
// of two orbiting free functions.
trait ToInputMods {
    fn to_input(self) -> Modifiers;
}

impl ToInputMods for platform::protocol::Modifiers {
    fn to_input(self) -> Modifiers {
        let any = self.depressed | self.latched | self.locked;
        let mut out = Modifiers::NONE;
        if any & 0x01 != 0 {
            out = out | Modifiers::SHIFT;
        }
        if any & 0x04 != 0 {
            out = out | Modifiers::CTRL;
        }
        if any & 0x08 != 0 {
            out = out | Modifiers::ALT;
        }
        if any & 0x40 != 0 {
            out = out | Modifiers::SUPER;
        }
        if self.locked & 0x02 != 0 {
            out = out | Modifiers::CAPS;
        }
        out
    }
}

trait ToInputKey {
    fn to_input(self) -> Option<Key>;
}

impl ToInputKey for Option<platform::protocol::Key> {
    #[allow(clippy::too_many_lines)]
    fn to_input(self) -> Option<Key> {
        use platform::protocol::Key as PK;
        Some(match self? {
            PK::A => Key::Char('a'),
            PK::B => Key::Char('b'),
            PK::C => Key::Char('c'),
            PK::D => Key::Char('d'),
            PK::E => Key::Char('e'),
            PK::F => Key::Char('f'),
            PK::G => Key::Char('g'),
            PK::H => Key::Char('h'),
            PK::I => Key::Char('i'),
            PK::J => Key::Char('j'),
            PK::K => Key::Char('k'),
            PK::L => Key::Char('l'),
            PK::M => Key::Char('m'),
            PK::N => Key::Char('n'),
            PK::O => Key::Char('o'),
            PK::P => Key::Char('p'),
            PK::Q => Key::Char('q'),
            PK::R => Key::Char('r'),
            PK::S => Key::Char('s'),
            PK::T => Key::Char('t'),
            PK::U => Key::Char('u'),
            PK::V => Key::Char('v'),
            PK::W => Key::Char('w'),
            PK::X => Key::Char('x'),
            PK::Y => Key::Char('y'),
            PK::Z => Key::Char('z'),

            PK::N1 => Key::Char('1'),
            PK::N2 => Key::Char('2'),
            PK::N3 => Key::Char('3'),
            PK::N4 => Key::Char('4'),
            PK::N5 => Key::Char('5'),
            PK::N6 => Key::Char('6'),
            PK::N7 => Key::Char('7'),
            PK::N8 => Key::Char('8'),
            PK::N9 => Key::Char('9'),
            PK::N0 => Key::Char('0'),

            PK::Minus => Key::Char('-'),
            PK::Equal => Key::Char('='),
            PK::LBracket => Key::Char('['),
            PK::RBracket => Key::Char(']'),
            PK::Backslash => Key::Char('\\'),
            PK::Semicolon => Key::Char(';'),
            PK::Apostrophe => Key::Char('\''),
            PK::Backtick => Key::Char('`'),
            PK::Comma => Key::Char(','),
            PK::Dot => Key::Char('.'),
            PK::Slash => Key::Char('/'),
            PK::Space => Key::Char(' '),

            PK::Enter => Key::Enter,
            PK::Backspace => Key::Backspace,
            PK::Tab => Key::Tab,
            PK::Esc => Key::Esc,
            PK::Delete => Key::Delete,
            PK::Insert => Key::Insert,

            PK::Up => Key::Up,
            PK::Down => Key::Down,
            PK::Left => Key::Left,
            PK::Right => Key::Right,
            PK::Home => Key::Home,
            PK::End => Key::End,
            PK::PageUp => Key::PageUp,
            PK::PageDown => Key::PageDown,

            PK::F1 => Key::F(1),
            PK::F2 => Key::F(2),
            PK::F3 => Key::F(3),
            PK::F4 => Key::F(4),
            PK::F5 => Key::F(5),
            PK::F6 => Key::F(6),
            PK::F7 => Key::F(7),
            PK::F8 => Key::F(8),
            PK::F9 => Key::F(9),
            PK::F10 => Key::F(10),
            PK::F11 => Key::F(11),
            PK::F12 => Key::F(12),

            PK::LShift | PK::RShift => Key::Shift,
            PK::LCtrl | PK::RCtrl => Key::Ctrl,
            PK::LAlt | PK::RAlt => Key::Alt,
            PK::LMeta | PK::RMeta => Key::Super,
            PK::CapsLock => Key::CapsLock,
        })
    }
}

#[cfg(test)]
mod key_translation_tests {
    use super::*;
    use platform::protocol::Key as PK;

    #[test]
    fn platform_key_none_input_gives_none() {
        assert_eq!(None::<PK>.to_input(), None);
    }

    #[test]
    fn platform_key_every_letter_maps_to_its_own_lowercase_char() {
        let pairs = [
            (PK::A, 'a'),
            (PK::B, 'b'),
            (PK::C, 'c'),
            (PK::D, 'd'),
            (PK::E, 'e'),
            (PK::F, 'f'),
            (PK::G, 'g'),
            (PK::H, 'h'),
            (PK::I, 'i'),
            (PK::J, 'j'),
            (PK::K, 'k'),
            (PK::L, 'l'),
            (PK::M, 'm'),
            (PK::N, 'n'),
            (PK::O, 'o'),
            (PK::P, 'p'),
            (PK::Q, 'q'),
            (PK::R, 'r'),
            (PK::S, 's'),
            (PK::T, 't'),
            (PK::U, 'u'),
            (PK::V, 'v'),
            (PK::W, 'w'),
            (PK::X, 'x'),
            (PK::Y, 'y'),
            (PK::Z, 'z'),
        ];
        for (k, c) in pairs {
            assert_eq!(Some(k).to_input(), Some(Key::Char(c)), "{k:?} -> {c:?}");
        }
    }

    #[test]
    fn platform_key_every_digit_maps_to_its_own_char() {
        let pairs = [
            (PK::N0, '0'),
            (PK::N1, '1'),
            (PK::N2, '2'),
            (PK::N3, '3'),
            (PK::N4, '4'),
            (PK::N5, '5'),
            (PK::N6, '6'),
            (PK::N7, '7'),
            (PK::N8, '8'),
            (PK::N9, '9'),
        ];
        for (k, c) in pairs {
            assert_eq!(Some(k).to_input(), Some(Key::Char(c)), "{k:?} -> {c:?}");
        }
    }

    #[test]
    fn platform_key_punctuation_and_space() {
        let pairs = [
            (PK::Minus, '-'),
            (PK::Equal, '='),
            (PK::LBracket, '['),
            (PK::RBracket, ']'),
            (PK::Backslash, '\\'),
            (PK::Semicolon, ';'),
            (PK::Apostrophe, '\''),
            (PK::Backtick, '`'),
            (PK::Comma, ','),
            (PK::Dot, '.'),
            (PK::Slash, '/'),
            (PK::Space, ' '),
        ];
        for (k, c) in pairs {
            assert_eq!(Some(k).to_input(), Some(Key::Char(c)), "{k:?} -> {c:?}");
        }
    }

    #[test]
    fn platform_key_named_keys() {
        assert_eq!(Some(PK::Enter).to_input(), Some(Key::Enter));
        assert_eq!(Some(PK::Backspace).to_input(), Some(Key::Backspace));
        assert_eq!(Some(PK::Tab).to_input(), Some(Key::Tab));
        assert_eq!(Some(PK::Esc).to_input(), Some(Key::Esc));
        assert_eq!(Some(PK::Delete).to_input(), Some(Key::Delete));
        assert_eq!(Some(PK::Insert).to_input(), Some(Key::Insert));
    }

    #[test]
    fn platform_key_navigation_cluster() {
        assert_eq!(Some(PK::Up).to_input(), Some(Key::Up));
        assert_eq!(Some(PK::Down).to_input(), Some(Key::Down));
        assert_eq!(Some(PK::Left).to_input(), Some(Key::Left));
        assert_eq!(Some(PK::Right).to_input(), Some(Key::Right));
        assert_eq!(Some(PK::Home).to_input(), Some(Key::Home));
        assert_eq!(Some(PK::End).to_input(), Some(Key::End));
        assert_eq!(Some(PK::PageUp).to_input(), Some(Key::PageUp));
        assert_eq!(Some(PK::PageDown).to_input(), Some(Key::PageDown));
    }

    #[test]
    fn platform_key_function_keys_1_through_12() {
        let pairs = [
            (PK::F1, 1),
            (PK::F2, 2),
            (PK::F3, 3),
            (PK::F4, 4),
            (PK::F5, 5),
            (PK::F6, 6),
            (PK::F7, 7),
            (PK::F8, 8),
            (PK::F9, 9),
            (PK::F10, 10),
            (PK::F11, 11),
            (PK::F12, 12),
        ];
        for (k, n) in pairs {
            assert_eq!(Some(k).to_input(), Some(Key::F(n)), "{k:?} -> F({n})");
        }
    }

    #[test]
    fn platform_key_modifier_keys_collapse_left_and_right_variants() {
        assert_eq!(Some(PK::LShift).to_input(), Some(Key::Shift));
        assert_eq!(Some(PK::RShift).to_input(), Some(Key::Shift));
        assert_eq!(Some(PK::LCtrl).to_input(), Some(Key::Ctrl));
        assert_eq!(Some(PK::RCtrl).to_input(), Some(Key::Ctrl));
        assert_eq!(Some(PK::LAlt).to_input(), Some(Key::Alt));
        assert_eq!(Some(PK::RAlt).to_input(), Some(Key::Alt));
        assert_eq!(Some(PK::LMeta).to_input(), Some(Key::Super));
        assert_eq!(Some(PK::RMeta).to_input(), Some(Key::Super));
        assert_eq!(Some(PK::CapsLock).to_input(), Some(Key::CapsLock));
    }

    #[test]
    fn platform_mods_none_when_nothing_set() {
        let m = platform::protocol::Modifiers::default();
        assert_eq!(m.to_input(), Modifiers::NONE);
    }

    #[test]
    fn platform_mods_shift_from_depressed_bit() {
        let m = platform::protocol::Modifiers {
            depressed: 0x01,
            ..Default::default()
        };
        assert_eq!(m.to_input(), Modifiers::SHIFT);
    }

    #[test]
    fn platform_mods_ctrl_from_latched_bit() {
        let m = platform::protocol::Modifiers {
            latched: 0x04,
            ..Default::default()
        };
        assert_eq!(m.to_input(), Modifiers::CTRL);
    }

    #[test]
    fn platform_mods_alt_from_locked_bit() {
        let m = platform::protocol::Modifiers {
            locked: 0x08,
            ..Default::default()
        };
        assert_eq!(m.to_input(), Modifiers::ALT);
    }

    #[test]
    fn platform_mods_super_bit() {
        let m = platform::protocol::Modifiers {
            depressed: 0x40,
            ..Default::default()
        };
        assert_eq!(m.to_input(), Modifiers::SUPER);
    }

    #[test]
    fn platform_mods_caps_lock_only_from_locked_not_depressed_or_latched() {
        // Caps Lock is special-cased to only count when *locked* (bit 0x02),
        // matching XKB's modifier semantics -- depressed/latched 0x02 must
        // NOT produce CAPS.
        let locked = platform::protocol::Modifiers {
            locked: 0x02,
            ..Default::default()
        };
        assert_eq!(locked.to_input(), Modifiers::CAPS);

        let depressed = platform::protocol::Modifiers {
            depressed: 0x02,
            ..Default::default()
        };
        assert_eq!(depressed.to_input(), Modifiers::NONE);

        let latched = platform::protocol::Modifiers {
            latched: 0x02,
            ..Default::default()
        };
        assert_eq!(latched.to_input(), Modifiers::NONE);
    }

    #[test]
    fn platform_mods_all_bits_combine() {
        let m = platform::protocol::Modifiers {
            depressed: 0x01 | 0x04 | 0x08 | 0x40,
            latched: 0,
            locked: 0x02,
            group: 0,
        };
        let out = m.to_input();
        assert_eq!(
            out,
            Modifiers::SHIFT
                | Modifiers::CTRL
                | Modifiers::ALT
                | Modifiers::SUPER
                | Modifiers::CAPS
        );
    }

    #[test]
    fn platform_mods_union_of_depressed_latched_locked_for_same_bit() {
        // Shift set only in latched (not depressed) must still register --
        // we track the union of all three fields, not just depressed.
        let m = platform::protocol::Modifiers {
            latched: 0x01,
            ..Default::default()
        };
        assert_eq!(m.to_input(), Modifiers::SHIFT);
    }

    #[test]
    fn resize_debounce_holds_until_quiet_then_releases_latest() {
        let mut d = ResizeDebounce::new();
        // Nothing pending yet.
        assert_eq!(d.take_if_settled(), None);
        assert_eq!(d.wait_cap_ms(), None);

        // A burst of drag steps: each re-arms the deadline, so none release
        // immediately (the quiet period hasn't elapsed).
        d.note(80, 24);
        d.note(40, 24);
        d.note(20, 24);
        assert_eq!(d.take_if_settled(), None, "must not release mid-drag");
        // A wait cap is advertised so the loop wakes to flush later.
        assert!(d.wait_cap_ms().is_some());

        // After the quiet period elapses, the LATEST size (not an
        // intermediate one) is released exactly once.
        std::thread::sleep(ResizeDebounce::QUIET + Duration::from_millis(20));
        assert_eq!(d.take_if_settled(), Some((20, 24)));
        // And it's consumed: a second poll yields nothing.
        assert_eq!(d.take_if_settled(), None);
        assert_eq!(d.wait_cap_ms(), None);
    }

    #[test]
    fn resize_debounce_deadline_pushes_forward_on_each_step() {
        let mut d = ResizeDebounce::new();
        d.note(80, 24);
        let first = d.wait_cap_ms().expect("pending after first note");
        // A later step should reset the countdown to roughly the full quiet
        // period again (>= the tail of the first), never shrink toward zero.
        std::thread::sleep(Duration::from_millis(10));
        d.note(40, 24);
        let second = d.wait_cap_ms().expect("pending after second note");
        assert!(
            second >= first - 10,
            "deadline did not re-arm: first={first} second={second}"
        );
    }
}
