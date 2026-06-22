//! `terminal` — entry point.
//!
//! M5 integration: PTY + vt_parser + grid + Wayland window wired through
//! `crates/input` for full VT / Kitty keyboard and mouse encoding.

use grid::Grid;
use input::{Encoder, Key, KeyState, Modifiers};
use platform::{InputEvent, Result, TermWindow};
use pty::PtySimple;
use vt_parser::Parser;

/// Initial terminal dimensions (cells).
const COLS: u16 = 80;
const ROWS: u16 = 24;

fn main() {
    if let Err(e) = run() {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // 1. Open the Wayland window.
    let mut win = TermWindow::new("terminal", COLS, ROWS)?;

    // 2. Spawn the PTY + shell.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut pty = PtySimple::spawn(&shell, &[], COLS, ROWS)
        .map_err(|e| platform::Error::Conn(platform::conn::ConnError::Io(
            std::io::Error::other(format!("pty: {e:?}")),
        )))?;

    // 3. Grid + VT parser + input encoder.
    let mut grid    = Grid::new(COLS, ROWS);
    let mut parser  = Parser::new();
    let mut encoder = Encoder::new();

    // 4. Event loop.
    loop {
        // PTY → parse → grid.
        let mut buf = [0u8; 4096];
        loop {
            match pty.try_read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    parser.feed(&buf[..n], |ev| {
                        // Sync encoder state from VT events that change input mode.
                        sync_encoder(&mut encoder, &ev);
                        grid.apply(&ev);
                    });
                }
            }
        }

        // Dirty snapshot → window.
        let snap = grid.take_dirty();
        if !snap.is_empty() {
            win.push_dirty(snap);
        }

        // Window input → PTY bytes.
        while let Some(ev) = win.next_input() {
            match ev {
                InputEvent::Close => return Ok(()),
                InputEvent::Resize { cols, rows } => {
                    grid.resize(cols, rows);
                    let _ = pty.resize(cols, rows);
                }
                InputEvent::Key { key, state, modifiers } => {
                    use platform::protocol::KeyState as PKeyState;
                    let kstate = match state {
                        Some(PKeyState::Pressed)  => KeyState::Pressed,
                        Some(PKeyState::Released) => KeyState::Released,
                        None                      => continue,
                    };
                    // Only encode press/repeat in legacy mode.
                    if kstate == KeyState::Released
                        && encoder.keyboard_mode == input::KeyboardMode::Legacy
                    {
                        continue;
                    }

                    let mods = platform_mods(modifiers);
                    let Some(ikey) = platform_key(key) else { continue };

                    let mut ibuf = [0u8; 64];
                    if let Some(n) = encoder.encode_key(ikey, kstate, mods, &mut ibuf) {
                        let _ = pty.write(&ibuf[..n]);
                    }
                }
            }
        }

        // Wayland tick.
        if !win.tick()? {
            break;
        }
    }

    Ok(())
}

/// Translate `platform::protocol::Modifiers` (raw XKB bitmask) → `input::Modifiers`.
fn platform_mods(m: platform::protocol::Modifiers) -> Modifiers {
    let any = m.depressed | m.latched | m.locked;
    let mut out = Modifiers::NONE;
    if any & 0x01 != 0 { out = out | Modifiers::SHIFT; }
    if any & 0x04 != 0 { out = out | Modifiers::CTRL; }
    if any & 0x08 != 0 { out = out | Modifiers::ALT; }
    if any & 0x40 != 0 { out = out | Modifiers::SUPER; }
    if m.locked & 0x02 != 0 { out = out | Modifiers::CAPS; }
    out
}

/// Map `platform::protocol::Key` (evdev codes) → `input::Key` (semantic).
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

        PK::Minus      => Key::Char('-'),
        PK::Equal      => Key::Char('='),
        PK::LBracket   => Key::Char('['),
        PK::RBracket   => Key::Char(']'),
        PK::Backslash  => Key::Char('\\'),
        PK::Semicolon  => Key::Char(';'),
        PK::Apostrophe => Key::Char('\''),
        PK::Backtick   => Key::Char('`'),
        PK::Comma      => Key::Char(','),
        PK::Dot        => Key::Char('.'),
        PK::Slash      => Key::Char('/'),
        PK::Space      => Key::Char(' '),

        PK::Enter     => Key::Enter,
        PK::Backspace => Key::Backspace,
        PK::Tab       => Key::Tab,
        PK::Esc       => Key::Esc,
        PK::Delete    => Key::Delete,
        PK::Insert    => Key::Insert,

        PK::Up    => Key::Up,
        PK::Down  => Key::Down,
        PK::Left  => Key::Left,
        PK::Right => Key::Right,
        PK::Home  => Key::Home,
        PK::End   => Key::End,
        PK::PageUp   => Key::PageUp,
        PK::PageDown => Key::PageDown,

        PK::F1  => Key::F(1),  PK::F2  => Key::F(2),
        PK::F3  => Key::F(3),  PK::F4  => Key::F(4),
        PK::F5  => Key::F(5),  PK::F6  => Key::F(6),
        PK::F7  => Key::F(7),  PK::F8  => Key::F(8),
        PK::F9  => Key::F(9),  PK::F10 => Key::F(10),
        PK::F11 => Key::F(11), PK::F12 => Key::F(12),

        PK::LShift | PK::RShift => Key::Shift,
        PK::LCtrl  | PK::RCtrl  => Key::Ctrl,
        PK::LAlt   | PK::RAlt   => Key::Alt,
        PK::LMeta  | PK::RMeta  => Key::Super,
        PK::CapsLock             => Key::CapsLock,
    })
}

/// Sync encoder state from VT events (app-cursor, app-keypad, mouse mode).
fn sync_encoder(enc: &mut Encoder, ev: &vt_parser::Event) {
    use vt_parser::{CsiAction, Event};
    let Event::Csi(csi) = ev else { return };
    match *csi {
        CsiAction::DecSet(mode) => apply_dec(enc, mode, true),
        CsiAction::DecRst(mode) => apply_dec(enc, mode, false),
        // SM/RM mode 1 = application keypad.
        CsiAction::SetMode(1)   => enc.app_keypad = true,
        CsiAction::ResetMode(1) => enc.app_keypad = false,
        _ => {}
    }
}

/// Apply a DEC private mode set/reset to the encoder.
fn apply_dec(enc: &mut Encoder, mode: u16, set: bool) {
    match mode {
        // DECCKM — application cursor keys.
        1    => enc.app_cursor = set,
        // Mouse tracking modes.
        9    => enc.mouse_mode = if set { input::MouseMode::X10 }          else { input::MouseMode::Off },
        1000 => enc.mouse_mode = if set { input::MouseMode::Normal }       else { input::MouseMode::Off },
        1002 => enc.mouse_mode = if set { input::MouseMode::ButtonMotion } else { input::MouseMode::Off },
        1003 => enc.mouse_mode = if set { input::MouseMode::AnyEvent }     else { input::MouseMode::Off },
        1006 => enc.mouse_mode = if set { input::MouseMode::Sgr }          else { input::MouseMode::Off },
        _    => {}
    }
}
