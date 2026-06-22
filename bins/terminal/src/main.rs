//! `terminal` — entry point.
//!
//! M4 integration: single-thread prototype wiring PTY + `vt_parser` + grid +
//! Wayland window together.  Full multi-thread ring topology comes in M5.

use grid::Grid;
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

    // 3. Grid + VT parser.
    let mut grid = Grid::new(COLS, ROWS);
    let mut parser = Parser::new();

    // 4. Event loop.
    loop {
        // PTY → parse → grid.
        let mut buf = [0u8; 4096];
        loop {
            match pty.try_read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    parser.feed(&buf[..n], |ev| grid.apply(&ev));
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
                    use platform::protocol::KeyState;
                    if state != Some(KeyState::Pressed) {
                        continue;
                    }
                    let shift = modifiers.depressed & 1 != 0 || modifiers.locked != 0;
                    let ctrl  = modifiers.depressed & 4 != 0;
                    if let Some(bytes) = key_to_bytes(key, shift, ctrl) {
                        let _ = pty.write(bytes);
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

/// Minimal US-QWERTY key → VT byte sequence.  Full encoding in M5 (`crates/input`).
fn key_to_bytes(
    key: Option<platform::protocol::Key>,
    shift: bool,
    ctrl: bool,
) -> Option<&'static [u8]> {
    use platform::protocol::Key;
    Some(match key? {
        // Control sequences.
        Key::Enter     => b"\r",
        Key::Backspace => b"\x7f",
        Key::Esc       => b"\x1b",
        Key::Tab       => b"\t",
        Key::Delete    => b"\x1b[3~",
        Key::Home      => b"\x1b[H",
        Key::End       => b"\x1b[F",
        Key::PageUp    => b"\x1b[5~",
        Key::PageDown  => b"\x1b[6~",
        Key::Up        => b"\x1b[A",
        Key::Down      => b"\x1b[B",
        Key::Right     => b"\x1b[C",
        Key::Left      => b"\x1b[D",
        Key::Insert    => b"\x1b[2~",
        Key::F1        => b"\x1bOP",
        Key::F2        => b"\x1bOQ",
        Key::F3        => b"\x1bOR",
        Key::F4        => b"\x1bOS",
        Key::F5        => b"\x1b[15~",
        Key::F6        => b"\x1b[17~",
        Key::F7        => b"\x1b[18~",
        Key::F8        => b"\x1b[19~",
        Key::F9        => b"\x1b[20~",
        Key::F10       => b"\x1b[21~",
        Key::F11       => b"\x1b[23~",
        Key::F12       => b"\x1b[24~",

        // Letter keys — ctrl overrides shift.
        Key::A => if ctrl { b"\x01" } else if shift { b"A" } else { b"a" },
        Key::B => if ctrl { b"\x02" } else if shift { b"B" } else { b"b" },
        Key::C => if ctrl { b"\x03" } else if shift { b"C" } else { b"c" },
        Key::D => if ctrl { b"\x04" } else if shift { b"D" } else { b"d" },
        Key::E => if ctrl { b"\x05" } else if shift { b"E" } else { b"e" },
        Key::F => if ctrl { b"\x06" } else if shift { b"F" } else { b"f" },
        Key::G => if ctrl { b"\x07" } else if shift { b"G" } else { b"g" },
        Key::H => if ctrl { b"\x08" } else if shift { b"H" } else { b"h" },
        Key::I => if ctrl { b"\x09" } else if shift { b"I" } else { b"i" },
        Key::J => if ctrl { b"\x0a" } else if shift { b"J" } else { b"j" },
        Key::K => if ctrl { b"\x0b" } else if shift { b"K" } else { b"k" },
        Key::L => if ctrl { b"\x0c" } else if shift { b"L" } else { b"l" },
        Key::M => if ctrl { b"\x0d" } else if shift { b"M" } else { b"m" },
        Key::N => if ctrl { b"\x0e" } else if shift { b"N" } else { b"n" },
        Key::O => if ctrl { b"\x0f" } else if shift { b"O" } else { b"o" },
        Key::P => if ctrl { b"\x10" } else if shift { b"P" } else { b"p" },
        Key::Q => if ctrl { b"\x11" } else if shift { b"Q" } else { b"q" },
        Key::R => if ctrl { b"\x12" } else if shift { b"R" } else { b"r" },
        Key::S => if ctrl { b"\x13" } else if shift { b"S" } else { b"s" },
        Key::T => if ctrl { b"\x14" } else if shift { b"T" } else { b"t" },
        Key::U => if ctrl { b"\x15" } else if shift { b"U" } else { b"u" },
        Key::V => if ctrl { b"\x16" } else if shift { b"V" } else { b"v" },
        Key::W => if ctrl { b"\x17" } else if shift { b"W" } else { b"w" },
        Key::X => if ctrl { b"\x18" } else if shift { b"X" } else { b"x" },
        Key::Y => if ctrl { b"\x19" } else if shift { b"Y" } else { b"y" },
        Key::Z => if ctrl { b"\x1a" } else if shift { b"Z" } else { b"z" },

        // Number row (unshifted — shifted symbols skipped for now).
        Key::N1 => if shift { b"!" } else { b"1" },
        Key::N2 => if shift { b"@" } else { b"2" },
        Key::N3 => if shift { b"#" } else { b"3" },
        Key::N4 => if shift { b"$" } else { b"4" },
        Key::N5 => if shift { b"%" } else { b"5" },
        Key::N6 => if shift { b"^" } else { b"6" },
        Key::N7 => if shift { b"&" } else { b"7" },
        Key::N8 => if shift { b"*" } else { b"8" },
        Key::N9 => if shift { b"(" } else { b"9" },
        Key::N0 => if shift { b")" } else { b"0" },

        Key::Minus       => if shift { b"_" } else { b"-" },
        Key::Equal       => if shift { b"+" } else { b"=" },
        Key::LBracket    => if shift { b"{" } else { b"[" },
        Key::RBracket    => if shift { b"}" } else { b"]" },
        Key::Backslash   => if shift { b"|" } else { b"\\" },
        Key::Semicolon   => if shift { b":" } else { b";" },
        Key::Apostrophe  => if shift { b"\"" } else { b"'" },
        Key::Backtick    => if shift { b"~" } else { b"`" },
        Key::Comma       => if shift { b"<" } else { b"," },
        Key::Dot         => if shift { b">" } else { b"." },
        Key::Slash       => if shift { b"?" } else { b"/" },
        Key::Space       => b" ",

        // Modifiers produce no bytes.
        Key::LShift | Key::RShift | Key::LCtrl | Key::RCtrl |
        Key::LAlt | Key::RAlt | Key::LMeta | Key::RMeta | Key::CapsLock => return None,
    })
}
