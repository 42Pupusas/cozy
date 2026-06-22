//! `terminal` — multi-threaded entry point.
//!
//! Thread topology (M8):
//!
//! ```text
//!  ┌────────────┐  SPSC<PtyChunk>   ┌──────────────┐  SPSC<DirtySnapshot>  ┌──────────────┐
//!  │  pty_thread│ ────────────────► │ parser_thread │ ─────────────────────► │  ui_thread   │
//!  │            │                   │ (vt_parser +  │                        │  (Wayland    │
//!  │ forkpty    │ ◄──────────────── │  grid)        │                        │   window)    │
//!  └────────────┘  SPSC<InputMsg>  └──────────────┘                        └──────┬───────┘
//!                  (key bytes /                                                     │
//!                   resize / quit)                                    SPSC<InputMsg>│
//!                                                                                  ▼
//!                                                                    pty_thread input drain
//! ```
//!
//! All rings are quetzalcoatl SPSC — no mutexes, no async runtime.
//! Config is loaded from `$XDG_CONFIG_HOME/terminal/config.toml` at startup.

#![allow(clippy::cast_possible_truncation)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use config::Config;
use grid::{DirtySnapshot, Grid};
use input::{Encoder, Key, KeyState, Modifiers};
use platform::{InputEvent, TermWindow};
use pty::PtySimple;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::RingBuffer;
use vt_parser::Parser;

/// Initial terminal dimensions (cells).
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// PTY read buffer size.
const PTY_BUF: usize = 4096;

// Ring topology (3 rings, one per thread edge of the pty→parser→ui→pty cycle):
//
//   parser_in  (ParserMsg)    pty + ui  → parser   MPSC  data chunks + resizes
//   dirty      (DirtySnapshot) parser   → ui       SPSC  render snapshots
//   input      (InputMsg)      ui + parser → pty    MPSC  keystrokes + CPR replies
//
/// Ring capacities (power-of-two items).
const RING_PARSER_IN: usize = 64; // ParserMsg slots     pty+ui → parser
const RING_DIRTY:     usize = 16; // DirtySnapshot slots parser → ui
const RING_INPUT:     usize = 64; // InputMsg slots      ui+parser → pty

// ── Messages ──────────────────────────────────────────────────────────────────

/// A raw PTY read chunk — heap-allocated to keep the ring slots small.
#[derive(Clone)]
struct PtyChunk {
    buf: Box<[u8; PTY_BUF]>,
    len: usize,
}

/// Messages into the parser/grid thread. Merged onto one MPSC ring: the pty
/// thread sends shell output (`Data`), the ui thread sends window resizes
/// (`Resize`) so the grid tracks the new cell extent and reflows.
#[derive(Clone)]
enum ParserMsg {
    Data(PtyChunk),
    Resize { cols: u16, rows: u16 },
}

/// Messages from the UI thread to the PTY thread.
#[derive(Clone)]
enum InputMsg {
    Bytes(Box<[u8]>),
    Resize { cols: u16, rows: u16 },
    Quit,
}

// ── Entry ──────────────────────────────────────────────────────────────────────

fn main() {
    let cfg = Config::load().unwrap_or_else(|e| {
        eprintln!("terminal: config error: {e} — using defaults");
        Config::default()
    });

    if let Err(e) = run(&cfg) {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    }
}

fn run(cfg: &Config) -> platform::Result<()> {
    // ── Rings ───────────────────────────────────────────────────────────────
    // parser_in is MPSC: the pty thread pushes shell output, the ui thread
    // pushes resizes. The ring's own park/unpark wakes the parked parser when
    // either producer pushes — so resizes wake the grid directly (no eventfd).
    let (parser_data_tx, parser_in_rx) =
        mpsc::RingBuffer::<ParserMsg>::new(Capacity::exact(RING_PARSER_IN)).split();
    let parser_resize_tx = parser_data_tx.clone();
    let (dirty_tx, dirty_rx) =
        RingBuffer::<DirtySnapshot>::new(Capacity::exact(RING_DIRTY)).split();
    // input is MPSC too: the ui thread (keystrokes / resize / quit) and the
    // parser/grid thread (CPR/DA responses written back to the shell) both feed
    // the single PTY consumer.
    let (input_tx, input_rx) =
        mpsc::RingBuffer::<InputMsg>::new(Capacity::exact(RING_INPUT)).split();
    let reply_tx = input_tx.clone();

    // ── Shared quit flag ────────────────────────────────────────────────────
    let quit        = Arc::new(AtomicBool::new(false));
    let quit_pty    = Arc::clone(&quit);
    let quit_parser = Arc::clone(&quit);

    // ── Cross-thread wakeups (eventfd + poll) ────────────────────────────────
    // Each thread that waits on more than one source blocks in poll(2) on an
    // eventfd instead of busy-spinning. `ui_wake` is owned by the window (the
    // Wayland reader signals it); the parser also signals it after a snapshot.
    // `pty_wake` lets the UI thread rouse the PTY thread when it queues input.
    let ui_wake  = platform::Waker::new().expect("create ui waker");
    let pty_wake = waker::Waker::new().expect("create pty waker");
    let parser_ui_wake  = ui_wake.clone();
    let parser_pty_wake = pty_wake.clone();
    let ui_pty_wake     = pty_wake.clone();

    // ── Spawn PTY thread ────────────────────────────────────────────────────
    let shell = cfg.shell.clone();
    let pty_thread = std::thread::Builder::new()
        .name("pty".into())
        .spawn(move || pty_thread(shell, parser_data_tx, input_rx, pty_wake, quit_pty))
        .expect("spawn pty thread");

    // ── Spawn parser + grid thread ──────────────────────────────────────────
    let parser_thread = std::thread::Builder::new()
        .name("parser".into())
        .spawn(move || {
            parser_thread(
                COLS, ROWS, parser_in_rx, dirty_tx,
                reply_tx, parser_ui_wake, parser_pty_wake, quit_parser,
            );
        })
        .expect("spawn parser thread");

    // ── UI thread (main thread) ─────────────────────────────────────────────
    ui_thread(cfg, dirty_rx, input_tx, parser_resize_tx, ui_wake, ui_pty_wake, &quit)?;

    pty_thread.join().ok();
    parser_thread.join().ok();
    Ok(())
}

// ── PTY thread ────────────────────────────────────────────────────────────────

// Thread entry points must *own* their ring endpoints and the quit `Arc`: the
// values are moved into a `'static` closure, and dropping the producer/consumer
// on thread exit is what closes the ring. Borrowing is structurally impossible.
#[allow(clippy::needless_pass_by_value)]
fn pty_thread(
    shell:      String,
    parser_tx:  mpsc::Producer<ParserMsg>,
    mut input_rx:   mpsc::Consumer<InputMsg>,
    input_wake: waker::Waker,
    quit:       Arc<AtomicBool>,
) {
    let mut pty = match PtySimple::spawn(&shell, &[], COLS, ROWS) {
        Ok(p)  => p,
        Err(e) => { eprintln!("pty: spawn failed: {e:?}"); return; }
    };
    let master_fd = pty.master_fd();
    let wake_fd = input_wake.raw_fd();

    while !quit.load(Ordering::Relaxed) {
        let mut did_work = false;

        // Drain the wake counter up-front, BEFORE reading the sources. This is
        // what closes the lost-wakeup race: any input the UI signals after we
        // read below but before we poll leaves the counter non-zero, so the
        // poll returns immediately instead of sleeping through it.
        input_wake.drain();

        // Drain all pending input messages first (writes are cheap). Resizes
        // are COALESCED: an interactive drag fires a storm of configure events,
        // but the shell only needs one SIGWINCH at the final width. Sending one
        // per event makes the shell repaint its prompt for widths that no
        // longer match the grid (which coalesces the same way on its side),
        // leaving duplicated/merged prompt rows behind. Apply only the last.
        let mut pending_resize: Option<(u16, u16)> = None;
        while let Some(msg) = input_rx.pop() {
            did_work = true;
            match msg {
                InputMsg::Bytes(b)          => { let _ = pty.write(&b); }
                InputMsg::Resize { cols, rows } => { pending_resize = Some((cols, rows)); }
                InputMsg::Quit              => {
                    quit.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
        if let Some((cols, rows)) = pending_resize {
            let _ = pty.resize(cols, rows);
        }

        // Read from PTY and forward to the parser ring.
        let mut buf = Box::new([0u8; PTY_BUF]);
        match pty.try_read(buf.as_mut()) {
            Ok(Some(0)) | Err(_) => {
                // Genuine EOF (shell exited) or a hard I/O error — signal everyone.
                quit.store(true, Ordering::Relaxed);
                return;
            }
            Ok(Some(n)) => {
                // Best-effort; drop if ring is full (parser thread is behind).
                let _ = parser_tx.push(ParserMsg::Data(PtyChunk { buf, len: n }));
                did_work = true;
            }
            // EAGAIN: no data yet — keep looping.
            Ok(None) => {}
        }

        // Idle: block in poll(2) on BOTH the PTY master fd (level-triggered, so
        // pending output always re-wakes) and the input-wake eventfd, instead
        // of busy-spinning. A 100ms cap bounds quit-flag check latency.
        if !did_work {
            let _ = waker::poll_two(master_fd, wake_fd, 100);
        }
    }
}

// ── Parser + grid thread ──────────────────────────────────────────────────────

// Feed one parser message to the grid, returning a pending resize (if any) for
// the caller to coalesce — only the final dimensions per batch matter.
fn feed_parser(grid: &mut Grid, parser: &mut Parser, msg: ParserMsg) -> Option<(u16, u16)> {
    match msg {
        ParserMsg::Data(chunk) => {
            parser.feed(&chunk.buf[..chunk.len], |ev| grid.apply(&ev));
            None
        }
        ParserMsg::Resize { cols, rows } => Some((cols, rows)),
    }
}

// owns ring endpoints (see pty_thread); arg count is inherent to a thread entry.
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn parser_thread(
    cols:       u16,
    rows:       u16,
    mut parser_rx: mpsc::Consumer<ParserMsg>,
    dirty_tx:   quetzalcoatl::spsc::Producer<DirtySnapshot>,
    reply_tx:   mpsc::Producer<InputMsg>,
    ui_wake:    waker::Waker,
    pty_wake:   waker::Waker,
    quit:       Arc<AtomicBool>,
) {
    let mut grid   = Grid::new(cols, rows);
    let mut parser = Parser::new();

    // Push any host-bound replies the grid produced (CPR/DA) to the PTY thread.
    let flush_replies = |grid: &mut Grid| {
        let replies = grid.take_replies();
        if !replies.is_empty() {
            let _ = reply_tx.push(InputMsg::Bytes(replies.into_boxed_slice()));
            pty_wake.signal();
        }
    };

    while !quit.load(Ordering::Relaxed) {
        // Block for the next message, then drain whatever else is queued so a
        // burst is handled in one batch. Resizes coalesce: only the last wins.
        let Some(first) = parser_rx.pop_block() else { break };
        let mut pending_resize = feed_parser(&mut grid, &mut parser, first);
        while let Some(msg) = parser_rx.pop() {
            if let Some(rz) = feed_parser(&mut grid, &mut parser, msg) {
                pending_resize = Some(rz);
            }
        }
        if let Some((cols, rows)) = pending_resize {
            grid.resize(cols, rows);
        }

        flush_replies(&mut grid);

        let snap = grid.take_dirty();
        if !snap.is_empty() {
            let _ = dirty_tx.push(snap);
            ui_wake.signal(); // UI polls an eventfd, not this ring
        }
    }
}

// ── UI thread (main thread) ───────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value)] // owns the input producer; see pty_thread
fn ui_thread(
    cfg:      &Config,
    mut dirty_rx: quetzalcoatl::spsc::Consumer<DirtySnapshot>,
    input_tx: mpsc::Producer<InputMsg>,
    resize_tx: mpsc::Producer<ParserMsg>,
    ui_wake:  platform::Waker,
    pty_wake: waker::Waker,
    quit:     &AtomicBool,
) -> platform::Result<()> {
    let theme = platform::Theme::from_palette(&cfg.palette);
    let mut win     = TermWindow::new("terminal", COLS, ROWS, ui_wake, theme)?;
    let encoder = Encoder::new();

    // Future: use cfg.cursor.blink / blink_rate in the renderer.
    let _ = (&cfg.cursor.blink, &cfg.cursor.blink_rate);

    loop {
        if quit.load(Ordering::Relaxed) { break; }

        let mut did_work = false;
        // True if we pushed input the PTY thread must be roused for.
        let mut sent_input = false;

        // Push dirty snapshots into the Wayland renderer.
        while let Some(snap) = dirty_rx.pop() {
            win.push_dirty(snap);
            did_work = true;
        }

        // Process window input events.
        while let Some(ev) = win.next_input() {
            did_work = true;
            match ev {
                InputEvent::Close => {
                    quit.store(true, Ordering::Relaxed);
                    let _ = input_tx.push(InputMsg::Quit);
                    pty_wake.signal();
                    return Ok(());
                }
                InputEvent::Resize { cols, rows } => {
                    // PTY gets the winsize (SIGWINCH); the grid gets the new
                    // extent to reflow. Pushing to the parser MPSC ring unparks
                    // the parser directly — no separate wake needed.
                    let _ = input_tx.push(InputMsg::Resize { cols, rows });
                    let _ = resize_tx.push(ParserMsg::Resize { cols, rows });
                    sent_input = true;
                }
                InputEvent::Key { key, state, modifiers } => {
                    use platform::protocol::KeyState as PKeyState;
                    let kstate = match state {
                        Some(PKeyState::Pressed)  => KeyState::Pressed,
                        Some(PKeyState::Released) => KeyState::Released,
                        None => continue,
                    };
                    if kstate == KeyState::Released
                        && encoder.keyboard_mode == input::KeyboardMode::Legacy
                    {
                        continue;
                    }
                    let mods = platform_mods(modifiers);
                    let Some(ikey) = platform_key(key) else { continue };
                    let mut ibuf = [0u8; 64];
                    if let Some(n) = encoder.encode_key(ikey, kstate, mods, &mut ibuf) {
                        let _ = input_tx.push(InputMsg::Bytes(ibuf[..n].into()));
                        sent_input = true;
                    }
                }
            }
        }

        // Rouse the PTY thread once for the whole batch of queued input.
        if sent_input {
            pty_wake.signal();
        }

        if !win.tick()? { break; }

        // Idle: block in poll(2) on the UI wakeup eventfd instead of
        // busy-spinning. The Wayland reader signals it after pushing protocol
        // events; the parser signals it after producing a dirty snapshot. The
        // 100ms cap bounds quit-flag latency. `win.wait` drains the counter
        // after waking; any signal that raced in during this iteration's reads
        // leaves the counter set, so the poll returns at once (no lost wakeup).
        if !did_work {
            win.wait(100);
        }
    }

    quit.store(true, Ordering::Relaxed);
    Ok(())
}

// ── Key / modifier translation ────────────────────────────────────────────────

fn platform_mods(m: platform::protocol::Modifiers) -> Modifiers {
    let any = m.depressed | m.latched | m.locked;
    let mut out = Modifiers::NONE;
    if any & 0x01 != 0      { out = out | Modifiers::SHIFT; }
    if any & 0x04 != 0      { out = out | Modifiers::CTRL;  }
    if any & 0x08 != 0      { out = out | Modifiers::ALT;   }
    if any & 0x40 != 0      { out = out | Modifiers::SUPER; }
    if m.locked & 0x02 != 0 { out = out | Modifiers::CAPS;  }
    out
}

#[allow(clippy::too_many_lines)]
fn platform_key(k: Option<platform::protocol::Key>) -> Option<Key> {
    use platform::protocol::Key as PK;
    Some(match k? {
        PK::A => Key::Char('a'), PK::B => Key::Char('b'), PK::C => Key::Char('c'),
        PK::D => Key::Char('d'), PK::E => Key::Char('e'), PK::F => Key::Char('f'),
        PK::G => Key::Char('g'), PK::H => Key::Char('h'), PK::I => Key::Char('i'),
        PK::J => Key::Char('j'), PK::K => Key::Char('k'), PK::L => Key::Char('l'),
        PK::M => Key::Char('m'), PK::N => Key::Char('n'), PK::O => Key::Char('o'),
        PK::P => Key::Char('p'), PK::Q => Key::Char('q'), PK::R => Key::Char('r'),
        PK::S => Key::Char('s'), PK::T => Key::Char('t'), PK::U => Key::Char('u'),
        PK::V => Key::Char('v'), PK::W => Key::Char('w'), PK::X => Key::Char('x'),
        PK::Y => Key::Char('y'), PK::Z => Key::Char('z'),

        PK::N1 => Key::Char('1'), PK::N2 => Key::Char('2'), PK::N3 => Key::Char('3'),
        PK::N4 => Key::Char('4'), PK::N5 => Key::Char('5'), PK::N6 => Key::Char('6'),
        PK::N7 => Key::Char('7'), PK::N8 => Key::Char('8'), PK::N9 => Key::Char('9'),
        PK::N0 => Key::Char('0'),

        PK::Minus      => Key::Char('-'), PK::Equal      => Key::Char('='),
        PK::LBracket   => Key::Char('['), PK::RBracket   => Key::Char(']'),
        PK::Backslash  => Key::Char('\\'),
        PK::Semicolon  => Key::Char(';'), PK::Apostrophe => Key::Char('\''),
        PK::Backtick   => Key::Char('`'), PK::Comma      => Key::Char(','),
        PK::Dot        => Key::Char('.'), PK::Slash      => Key::Char('/'),
        PK::Space      => Key::Char(' '),

        PK::Enter     => Key::Enter,     PK::Backspace => Key::Backspace,
        PK::Tab       => Key::Tab,       PK::Esc       => Key::Esc,
        PK::Delete    => Key::Delete,    PK::Insert    => Key::Insert,

        PK::Up    => Key::Up,        PK::Down  => Key::Down,
        PK::Left  => Key::Left,      PK::Right => Key::Right,
        PK::Home  => Key::Home,      PK::End   => Key::End,
        PK::PageUp   => Key::PageUp, PK::PageDown => Key::PageDown,

        PK::F1  => Key::F(1),  PK::F2  => Key::F(2),  PK::F3  => Key::F(3),
        PK::F4  => Key::F(4),  PK::F5  => Key::F(5),  PK::F6  => Key::F(6),
        PK::F7  => Key::F(7),  PK::F8  => Key::F(8),  PK::F9  => Key::F(9),
        PK::F10 => Key::F(10), PK::F11 => Key::F(11), PK::F12 => Key::F(12),

        PK::LShift | PK::RShift => Key::Shift,
        PK::LCtrl  | PK::RCtrl  => Key::Ctrl,
        PK::LAlt   | PK::RAlt   => Key::Alt,
        PK::LMeta  | PK::RMeta  => Key::Super,
        PK::CapsLock             => Key::CapsLock,
    })
}
