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
use pty::{Pty, PtyInput, WinSize};
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
const RING_DIRTY: usize = 16; // DirtySnapshot slots parser → ui
const RING_INPUT: usize = 64; // InputMsg slots      ui+parser → pty

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
    // input is MPSC too: the ui thread (keystrokes / resize / quit) and the
    // parser/grid thread (CPR/DA responses written back to the shell) both feed
    // the single PTY consumer.
    let (input_tx, input_rx) =
        mpsc::RingBuffer::<InputMsg>::new(Capacity::exact(RING_INPUT)).split();
    let reply_tx = input_tx.clone();

    // ── Shared quit flag ────────────────────────────────────────────────────
    let quit = Arc::new(AtomicBool::new(false));
    let quit_pty = Arc::clone(&quit);
    let quit_parser = Arc::clone(&quit);

    // Bracketed-paste mode (DECSET 2004) is set by the app on the parser
    // thread (which owns the grid) but read on the UI thread (which writes
    // clipboard paste to the PTY). Published via a shared atomic.
    let bracketed_paste = Arc::new(AtomicBool::new(false));
    let bracketed_paste_parser = Arc::clone(&bracketed_paste);
    let bracketed_paste_ui = Arc::clone(&bracketed_paste);

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
                COLS,
                ROWS,
                parser_in_rx,
                dirty_tx,
                reply_tx,
                parser_ui_wake,
                parser_pty_wake,
                bracketed_paste_parser,
                quit_parser,
            );
        })
        .expect("spawn parser thread");

    // ── UI thread (main thread) ─────────────────────────────────────────────
    ui_thread(
        cfg,
        dirty_rx,
        input_tx,
        parser_resize_tx.clone(),
        parser_resize_tx,
        ui_wake,
        ui_pty_wake,
        &bracketed_paste_ui,
        &quit,
    )?;

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
    shell: String,
    parser_tx: mpsc::Producer<ParserMsg>,
    mut input_rx: mpsc::Consumer<InputMsg>,
    input_wake: waker::Waker,
    quit: Arc<AtomicBool>,
) {
    // All PTY I/O goes through io_uring: the master fd, the SIGCHLD
    // signalfd, and the input-wake eventfd each keep one read SQE armed, and
    // the loop parks in io_uring_enter until any of them completes — no
    // poll(2), no 100ms tick. The UI/parser threads signal `input_wake`
    // after pushing to `input_rx`, which completes the eventfd SQE.
    let win = WinSize {
        rows: ROWS,
        cols: COLS,
        xpixel: 0,
        ypixel: 0,
    };
    let pty = match Pty::spawn(&shell, &[], win, input_wake.raw_fd(), Arc::clone(&quit)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pty: spawn failed: {e}");
            quit.store(true, Ordering::Relaxed);
            return;
        }
    };

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

    // Adapter: PTY output chunks → parser ring. Best-effort push: if the
    // ring is full the parser is lagging; drop the chunk.
    let on_output = move |bytes: &[u8]| {
        for part in bytes.chunks(PTY_BUF) {
            let mut buf = Box::new([0u8; PTY_BUF]);
            buf[..part.len()].copy_from_slice(part);
            let _ = parser_tx.push(ParserMsg::Data(PtyChunk {
                buf,
                len: part.len(),
            }));
        }
    };

    match pty.run(next_input, on_output) {
        Ok(_code) => {}
        Err(e) => eprintln!("pty: event loop failed: {e}"),
    }
    // Shell exited (or loop errored) — signal everyone to shut down.
    quit.store(true, Ordering::Relaxed);
}

// ── Parser + grid thread ──────────────────────────────────────────────────────

// Feed one parser message to the grid, applying it immediately. Order matters:
// a Resize must take effect before any Data that follows it (the shell's
// post-SIGWINCH repaint must land on the already-reflowed grid), so resizes are
// NOT deferred or coalesced across interleaved data.
fn feed_parser(grid: &mut Grid, parser: &mut Parser, msg: ParserMsg) {
    match msg {
        ParserMsg::Data(chunk) => {
            parser.feed(&chunk.buf[..chunk.len], |ev| grid.apply(&ev));
        }
        ParserMsg::Resize { cols, rows } => grid.resize(cols, rows),
        ParserMsg::Scroll { delta } => grid.scroll_viewport(delta),
    }
}

// owns ring endpoints (see pty_thread); arg count is inherent to a thread entry.
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn parser_thread(
    cols: u16,
    rows: u16,
    mut parser_rx: mpsc::Consumer<ParserMsg>,
    dirty_tx: quetzalcoatl::spsc::Producer<DirtySnapshot>,
    reply_tx: mpsc::Producer<InputMsg>,
    ui_wake: waker::Waker,
    pty_wake: waker::Waker,
    bracketed_paste: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
) {
    let mut grid = Grid::new(cols, rows);
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
        // burst is handled in one batch — but strictly in arrival order, so a
        // Resize and the Data that follows it stay correctly sequenced.
        let Some(first) = parser_rx.pop_block() else {
            break;
        };
        feed_parser(&mut grid, &mut parser, first);
        while let Some(msg) = parser_rx.pop() {
            feed_parser(&mut grid, &mut parser, msg);
        }

        flush_replies(&mut grid);

        // Publish bracketed-paste mode for the UI thread (it wraps clipboard
        // paste in the 200~/201~ markers when the app asked for it).
        bracketed_paste.store(grid.bracketed_paste(), Ordering::Relaxed);

        let snap = grid.take_dirty();
        if !snap.is_empty() {
            let _ = dirty_tx.push(snap);
            ui_wake.signal(); // UI polls an eventfd, not this ring
        }
    }
}

// ── UI thread (main thread) ───────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)] // owns producers
fn ui_thread(
    cfg: &Config,
    mut dirty_rx: quetzalcoatl::spsc::Consumer<DirtySnapshot>,
    input_tx: mpsc::Producer<InputMsg>,
    resize_tx: mpsc::Producer<ParserMsg>,
    scroll_tx: mpsc::Producer<ParserMsg>,
    ui_wake: platform::Waker,
    pty_wake: waker::Waker,
    bracketed_paste: &AtomicBool,
    quit: &AtomicBool,
) -> platform::Result<()> {
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
        period_ms: (cfg.cursor.blink_rate * 2) as f32,
    };
    // TermWindow reads the output scale and calls this closure to rasterize
    // the font at native density (HiDPI-aware).
    let mut win = TermWindow::new("terminal", COLS, ROWS, ui_wake, cursor_cfg, move |scale| {
        platform::Renderer::with_scale(theme, &font, scale).with_opacity(opacity)
    })?;
    let encoder = Encoder::new();

    // Idle poll cap while an animation (cursor slide/blink) is in flight, in
    // milliseconds -- derived from `[window] target_fps`. This is a *target*
    // poll cadence, not a guaranteed frame rate: the actual ceiling is the
    // compositor's own vsync (`wl_callback.done` gates every present in
    // `TermWindow::maybe_present`), so on a 60Hz output setting this above 60
    // just means the loop wakes more often than it can usefully present,
    // spending a little extra CPU for lower input-to-animation latency. On a
    // 90/120/144Hz+ output it lets the animation actually track the higher
    // refresh rate. `target_fps` is validated to `1..=1000` by config, so
    // this is always `>= 1`.
    let anim_poll_cap_ms = (1000 / cfg.window.target_fps.max(1)).max(1).cast_signed();

    // Track the grid's current row count for page-scroll sizing (updated on
    // resize). Seeded from the startup dimensions.
    let mut grid_rows = ROWS;

    loop {
        if quit.load(Ordering::Relaxed) {
            break;
        }

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
                    grid_rows = rows;
                    // PTY gets the winsize (SIGWINCH); the grid gets the new
                    // extent to reflow. Pushing to the parser MPSC ring unparks
                    // the parser directly — no separate wake needed.
                    let _ = input_tx.push(InputMsg::Resize { cols, rows });
                    let _ = resize_tx.push(ParserMsg::Resize { cols, rows });
                    sent_input = true;
                }
                InputEvent::Scroll { lines } => {
                    // Mouse wheel scrolls the grid viewport (into scrollback),
                    // not the shell. Route to the parser thread, which owns the
                    // grid and re-renders the moved viewport.
                    let _ = scroll_tx.push(ParserMsg::Scroll { delta: lines });
                }
                InputEvent::Paste(bytes) => {
                    // Clipboard contents from another client: write to the PTY
                    // so the shell/app receives them as input. When the app
                    // enabled bracketed paste (DECSET 2004), wrap in the
                    // 200~/201~ markers so pasted newlines are treated as text,
                    // not as typed Enter (fish/zsh/neovim rely on this).
                    let data = if bracketed_paste.load(Ordering::Relaxed) {
                        let mut wrapped = Vec::with_capacity(bytes.len() + 12);
                        wrapped.extend_from_slice(b"\x1b[200~");
                        wrapped.extend_from_slice(&bytes);
                        wrapped.extend_from_slice(b"\x1b[201~");
                        wrapped
                    } else {
                        bytes
                    };
                    let _ = input_tx.push(InputMsg::Bytes(data.into_boxed_slice()));
                    sent_input = true;
                }
                InputEvent::Key {
                    key,
                    state,
                    modifiers,
                } => {
                    use platform::protocol::KeyState as PKeyState;
                    let kstate = match state {
                        Some(PKeyState::Pressed) => KeyState::Pressed,
                        Some(PKeyState::Released) => KeyState::Released,
                        None => continue,
                    };
                    if kstate == KeyState::Released
                        && encoder.keyboard_mode == input::KeyboardMode::Legacy
                    {
                        continue;
                    }
                    let mods = platform_mods(modifiers);
                    let Some(ikey) = platform_key(key) else {
                        continue;
                    };
                    // Shift+PgUp/PgDn scroll the grid viewport by a page,
                    // instead of sending the escape to the shell (bare
                    // PgUp/PgDn still go to the shell for history search).
                    if kstate == KeyState::Pressed && mods.shift() {
                        let page = i32::from(grid_rows);
                        match ikey {
                            Key::PageUp => {
                                let _ = scroll_tx.push(ParserMsg::Scroll { delta: page });
                                continue;
                            }
                            Key::PageDown => {
                                let _ = scroll_tx.push(ParserMsg::Scroll { delta: -page });
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // Ctrl+Shift+C / Ctrl+Shift+V: copy / paste via the system
                    // clipboard (handled by the window, not sent to the shell).
                    if kstate == KeyState::Pressed && mods.ctrl() && mods.shift() {
                        match ikey {
                            Key::Char('c') => {
                                win.copy_selection();
                                continue;
                            }
                            Key::Char('v') => {
                                win.request_paste();
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let mut ibuf = [0u8; 64];
                    if let Some(n) = encoder.encode_key(ikey, kstate, mods, &mut ibuf) {
                        let _ = input_tx.push(InputMsg::Bytes(ibuf[..n].into()));
                        sent_input = true;
                        // Any real keystroke snaps the cursor blink back to
                        // full opacity -- it should never be caught mid-fade
                        // by what you just typed.
                        win.notify_activity();
                    }
                }
            }
        }

        // Rouse the PTY thread once for the whole batch of queued input.
        if sent_input {
            pty_wake.signal();
        }

        if !win.tick()? {
            break;
        }

        // Idle: block in poll(2) on the UI wakeup eventfd instead of
        // busy-spinning. The Wayland reader signals it after pushing protocol
        // events; the parser signals it after producing a dirty snapshot.
        // `win.wait` drains the counter after waking; any signal that raced in
        // during this iteration's reads leaves the counter set, so the poll
        // returns at once (no lost wakeup).
        //
        // While the cursor is mid-slide or blinking-and-visible, cap the wait
        // at ANIM_POLL_CAP_MS instead of the normal 100ms so the animation
        // actually advances smoothly instead of updating in big 100ms jumps;
        // otherwise it stays the full 100ms, bounding only quit-flag latency
        // as before.
        if !did_work {
            let cap = if win.wants_animation_frame() {
                anim_poll_cap_ms
            } else {
                100
            };
            win.wait(cap);
        }
    }

    quit.store(true, Ordering::Relaxed);
    Ok(())
}

// ── Key / modifier translation ────────────────────────────────────────────────

fn platform_mods(m: platform::protocol::Modifiers) -> Modifiers {
    let any = m.depressed | m.latched | m.locked;
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
    if m.locked & 0x02 != 0 {
        out = out | Modifiers::CAPS;
    }
    out
}

#[allow(clippy::too_many_lines)]
fn platform_key(k: Option<platform::protocol::Key>) -> Option<Key> {
    use platform::protocol::Key as PK;
    Some(match k? {
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

#[cfg(test)]
mod key_translation_tests {
    use super::*;
    use platform::protocol::Key as PK;

    #[test]
    fn platform_key_none_input_gives_none() {
        assert_eq!(platform_key(None), None);
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
            assert_eq!(platform_key(Some(k)), Some(Key::Char(c)), "{k:?} -> {c:?}");
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
            assert_eq!(platform_key(Some(k)), Some(Key::Char(c)), "{k:?} -> {c:?}");
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
            assert_eq!(platform_key(Some(k)), Some(Key::Char(c)), "{k:?} -> {c:?}");
        }
    }

    #[test]
    fn platform_key_named_keys() {
        assert_eq!(platform_key(Some(PK::Enter)), Some(Key::Enter));
        assert_eq!(platform_key(Some(PK::Backspace)), Some(Key::Backspace));
        assert_eq!(platform_key(Some(PK::Tab)), Some(Key::Tab));
        assert_eq!(platform_key(Some(PK::Esc)), Some(Key::Esc));
        assert_eq!(platform_key(Some(PK::Delete)), Some(Key::Delete));
        assert_eq!(platform_key(Some(PK::Insert)), Some(Key::Insert));
    }

    #[test]
    fn platform_key_navigation_cluster() {
        assert_eq!(platform_key(Some(PK::Up)), Some(Key::Up));
        assert_eq!(platform_key(Some(PK::Down)), Some(Key::Down));
        assert_eq!(platform_key(Some(PK::Left)), Some(Key::Left));
        assert_eq!(platform_key(Some(PK::Right)), Some(Key::Right));
        assert_eq!(platform_key(Some(PK::Home)), Some(Key::Home));
        assert_eq!(platform_key(Some(PK::End)), Some(Key::End));
        assert_eq!(platform_key(Some(PK::PageUp)), Some(Key::PageUp));
        assert_eq!(platform_key(Some(PK::PageDown)), Some(Key::PageDown));
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
            assert_eq!(platform_key(Some(k)), Some(Key::F(n)), "{k:?} -> F({n})");
        }
    }

    #[test]
    fn platform_key_modifier_keys_collapse_left_and_right_variants() {
        assert_eq!(platform_key(Some(PK::LShift)), Some(Key::Shift));
        assert_eq!(platform_key(Some(PK::RShift)), Some(Key::Shift));
        assert_eq!(platform_key(Some(PK::LCtrl)), Some(Key::Ctrl));
        assert_eq!(platform_key(Some(PK::RCtrl)), Some(Key::Ctrl));
        assert_eq!(platform_key(Some(PK::LAlt)), Some(Key::Alt));
        assert_eq!(platform_key(Some(PK::RAlt)), Some(Key::Alt));
        assert_eq!(platform_key(Some(PK::LMeta)), Some(Key::Super));
        assert_eq!(platform_key(Some(PK::RMeta)), Some(Key::Super));
        assert_eq!(platform_key(Some(PK::CapsLock)), Some(Key::CapsLock));
    }

    #[test]
    fn platform_mods_none_when_nothing_set() {
        let m = platform::protocol::Modifiers::default();
        assert_eq!(platform_mods(m), Modifiers::NONE);
    }

    #[test]
    fn platform_mods_shift_from_depressed_bit() {
        let m = platform::protocol::Modifiers {
            depressed: 0x01,
            ..Default::default()
        };
        assert_eq!(platform_mods(m), Modifiers::SHIFT);
    }

    #[test]
    fn platform_mods_ctrl_from_latched_bit() {
        let m = platform::protocol::Modifiers {
            latched: 0x04,
            ..Default::default()
        };
        assert_eq!(platform_mods(m), Modifiers::CTRL);
    }

    #[test]
    fn platform_mods_alt_from_locked_bit() {
        let m = platform::protocol::Modifiers {
            locked: 0x08,
            ..Default::default()
        };
        assert_eq!(platform_mods(m), Modifiers::ALT);
    }

    #[test]
    fn platform_mods_super_bit() {
        let m = platform::protocol::Modifiers {
            depressed: 0x40,
            ..Default::default()
        };
        assert_eq!(platform_mods(m), Modifiers::SUPER);
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
        assert_eq!(platform_mods(locked), Modifiers::CAPS);

        let depressed = platform::protocol::Modifiers {
            depressed: 0x02,
            ..Default::default()
        };
        assert_eq!(platform_mods(depressed), Modifiers::NONE);

        let latched = platform::protocol::Modifiers {
            latched: 0x02,
            ..Default::default()
        };
        assert_eq!(platform_mods(latched), Modifiers::NONE);
    }

    #[test]
    fn platform_mods_all_bits_combine() {
        let m = platform::protocol::Modifiers {
            depressed: 0x01 | 0x04 | 0x08 | 0x40,
            latched: 0,
            locked: 0x02,
            group: 0,
        };
        let out = platform_mods(m);
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
        assert_eq!(platform_mods(m), Modifiers::SHIFT);
    }
}
