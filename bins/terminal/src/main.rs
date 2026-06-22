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
use quetzalcoatl::spsc::RingBuffer;
use vt_parser::Parser;

/// Initial terminal dimensions (cells).
const COLS: u16 = 80;
const ROWS: u16 = 24;

/// PTY read buffer size.
const PTY_BUF: usize = 4096;

/// Ring capacities (power-of-two items).
const RING_PTY_OUT: usize = 64; // PtyChunk slots       pty    → parser
const RING_DIRTY:   usize = 16; // DirtySnapshot slots  parser → ui
const RING_INPUT:   usize = 64; // InputMsg slots       ui     → pty

// ── Messages ──────────────────────────────────────────────────────────────────

/// A raw PTY read chunk — heap-allocated to keep the ring slots small.
#[derive(Clone)]
struct PtyChunk {
    buf: Box<[u8; PTY_BUF]>,
    len: usize,
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
    let (pty_out_tx, pty_out_rx) =
        RingBuffer::<PtyChunk>::new(Capacity::exact(RING_PTY_OUT)).split();
    let (dirty_tx, dirty_rx) =
        RingBuffer::<DirtySnapshot>::new(Capacity::exact(RING_DIRTY)).split();
    let (input_tx, input_rx) =
        RingBuffer::<InputMsg>::new(Capacity::exact(RING_INPUT)).split();

    // ── Shared quit flag ────────────────────────────────────────────────────
    let quit        = Arc::new(AtomicBool::new(false));
    let quit_pty    = Arc::clone(&quit);
    let quit_parser = Arc::clone(&quit);

    // ── Spawn PTY thread ────────────────────────────────────────────────────
    let shell = cfg.shell.clone();
    let pty_thread = std::thread::Builder::new()
        .name("pty".into())
        .spawn(move || pty_thread(shell, pty_out_tx, input_rx, quit_pty))
        .expect("spawn pty thread");

    // ── Spawn parser + grid thread ──────────────────────────────────────────
    let parser_thread = std::thread::Builder::new()
        .name("parser".into())
        .spawn(move || {
            parser_thread(COLS, ROWS, pty_out_rx, dirty_tx, quit_parser);
        })
        .expect("spawn parser thread");

    // ── UI thread (main thread) ─────────────────────────────────────────────
    ui_thread(cfg, dirty_rx, input_tx, &quit)?;

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
    pty_out_tx: quetzalcoatl::spsc::Producer<PtyChunk>,
    mut input_rx:   quetzalcoatl::spsc::Consumer<InputMsg>,
    quit:       Arc<AtomicBool>,
) {
    let mut pty = match PtySimple::spawn(&shell, &[], COLS, ROWS) {
        Ok(p)  => p,
        Err(e) => { eprintln!("pty: spawn failed: {e:?}"); return; }
    };

    while !quit.load(Ordering::Relaxed) {
        // Drain all pending input messages first (writes are cheap).
        while let Some(msg) = input_rx.pop() {
            match msg {
                InputMsg::Bytes(b)          => { let _ = pty.write(&b); }
                InputMsg::Resize { cols, rows } => { let _ = pty.resize(cols, rows); }
                InputMsg::Quit              => {
                    quit.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }

        // Read from PTY and forward to the parser ring.
        let mut buf = Box::new([0u8; PTY_BUF]);
        match pty.try_read(buf.as_mut()) {
            Ok(0) | Err(_) => {
                // Shell exited — signal everyone.
                quit.store(true, Ordering::Relaxed);
                return;
            }
            Ok(n) => {
                // Best-effort; drop if ring is full (parser thread is behind).
                let _ = pty_out_tx.push(PtyChunk { buf, len: n });
            }
        }

        std::thread::yield_now();
    }
}

// ── Parser + grid thread ──────────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value)] // owns ring endpoints; see pty_thread
fn parser_thread(
    cols:       u16,
    rows:       u16,
    mut pty_out_rx: quetzalcoatl::spsc::Consumer<PtyChunk>,
    dirty_tx:   quetzalcoatl::spsc::Producer<DirtySnapshot>,
    quit:       Arc<AtomicBool>,
) {
    let mut grid   = Grid::new(cols, rows);
    let mut parser = Parser::new();

    while !quit.load(Ordering::Relaxed) {
        let mut dirty = false;

        while let Some(chunk) = pty_out_rx.pop() {
            parser.feed(&chunk.buf[..chunk.len], |ev| { grid.apply(&ev); });
            dirty = true;
        }

        if dirty {
            let snap = grid.take_dirty();
            if !snap.is_empty() {
                let _ = dirty_tx.push(snap);
            }
        }

        std::thread::yield_now();
    }
}

// ── UI thread (main thread) ───────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value)] // owns the input producer; see pty_thread
fn ui_thread(
    cfg:      &Config,
    mut dirty_rx: quetzalcoatl::spsc::Consumer<DirtySnapshot>,
    input_tx: quetzalcoatl::spsc::Producer<InputMsg>,
    quit:     &AtomicBool,
) -> platform::Result<()> {
    let mut win     = TermWindow::new("terminal", COLS, ROWS)?;
    let encoder = Encoder::new();

    // Future: use cfg.cursor.blink / blink_rate in the renderer.
    let _ = (&cfg.cursor.blink, &cfg.cursor.blink_rate);

    loop {
        if quit.load(Ordering::Relaxed) { break; }

        // Push dirty snapshots into the Wayland renderer.
        while let Some(snap) = dirty_rx.pop() {
            win.push_dirty(snap);
        }

        // Process window input events.
        while let Some(ev) = win.next_input() {
            match ev {
                InputEvent::Close => {
                    quit.store(true, Ordering::Relaxed);
                    let _ = input_tx.push(InputMsg::Quit);
                    return Ok(());
                }
                InputEvent::Resize { cols, rows } => {
                    let _ = input_tx.push(InputMsg::Resize { cols, rows });
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
                    }
                }
            }
        }

        if !win.tick()? { break; }
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
