//! VT byte-sequence encoding for keyboard and mouse events.
//!
//! The [`Encoder`] holds the negotiated protocol state (keyboard mode, mouse
//! mode, application-cursor/keypad flags) and produces byte sequences into a
//! caller-supplied `&mut [u8]` buffer.  No heap allocation.

use crate::key::{Key, KeyState, Modifiers};
use crate::mouse::{ButtonState, MouseEvent, MouseMode};

/// Which keyboard-encoding protocol is active.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    /// Classic VT220 / xterm sequences (default).
    #[default]
    Legacy,
    /// `xterm` `modifyOtherKeys` level 2 (`CSI > 4 ; 2 m`).
    XtermModifyOtherKeys,
    /// Kitty progressive-enhancement protocol (`CSI = 1 u`).
    Kitty,
}

/// Stateful encoder: holds negotiated protocol flags and produces byte sequences.
#[derive(Clone, Debug, Default)]
pub struct Encoder {
    pub keyboard_mode:    KeyboardMode,
    pub mouse_mode:       MouseMode,
    /// Application-cursor mode (`CSI ? 1 h`): arrow keys send `SS3` instead of `CSI`.
    pub app_cursor:       bool,
    /// Application-keypad mode (`CSI = h`): keypad sends application sequences.
    pub app_keypad:       bool,
    /// Kitty progressive enhancement flags bitmask (only meaningful in Kitty mode).
    pub kitty_flags:      u8,
}

impl Encoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Encode a keyboard event into `buf`.  Returns the number of bytes written,
    /// or `None` if the key produces no output (modifier-only in legacy mode,
    /// or `Released`/`Repeated` in legacy mode for most keys).
    pub fn encode_key(
        &self,
        key:   Key,
        state: KeyState,
        mods:  Modifiers,
        buf:   &mut [u8],
    ) -> Option<usize> {
        // Kitty mode reports all state transitions; legacy only cares about press/repeat.
        let report = match self.keyboard_mode {
            KeyboardMode::Kitty => true,
            _ => matches!(state, KeyState::Pressed | KeyState::Repeated),
        };
        if !report {
            return None;
        }
        if key.is_modifier_only() && self.keyboard_mode != KeyboardMode::Kitty {
            return None;
        }

        match self.keyboard_mode {
            KeyboardMode::Legacy              => self.encode_legacy(key, mods, buf),
            KeyboardMode::XtermModifyOtherKeys => self.encode_xterm_modify(key, mods, buf),
            KeyboardMode::Kitty               => self.encode_kitty(key, state, mods, buf),
        }
    }

    /// Encode a mouse event into `buf`.  Returns the number of bytes written,
    /// or `None` if mouse reporting is off.
    pub fn encode_mouse(&self, ev: MouseEvent, buf: &mut [u8]) -> Option<usize> {
        match self.mouse_mode {
            MouseMode::Off => None,
            MouseMode::Sgr => Some(encode_mouse_sgr(ev, buf)),
            _ => Some(self.encode_mouse_x10(ev, buf)),
        }
    }

    // ── Legacy encoding ───────────────────────────────────────────────────────

    fn encode_legacy(&self, key: Key, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        // Ctrl+letter → C0 control codes.
        if mods.ctrl() {
            if let Key::Char(c) = key {
                if let Some(byte) = ctrl_byte(c) {
                    return Some(write1(buf, byte));
                }
            }
        }

        Some(match key {
            // ── Printable characters ──────────────────────────────────────
            Key::Char(c) => {
                // Alt prefixes with ESC.
                let mut n = 0;
                if mods.alt() {
                    buf[n] = b'\x1b';
                    n += 1;
                }
                let s = encode_char(c, mods.shift(), mods.caps());
                let src = s.as_bytes();
                buf[n..n + src.len()].copy_from_slice(src);
                n + src.len()
            }

            // ── Enter / Backspace / Tab / Esc ────────────────────────────
            Key::Enter     => write1(buf, b'\r'),
            Key::Backspace => write1(buf, b'\x7f'),
            Key::Tab       => {
                if mods.shift() {
                    writeb(buf, b"\x1b[Z")
                } else {
                    write1(buf, b'\t')
                }
            }
            Key::Esc       => write1(buf, b'\x1b'),

            // ── Delete / Insert ──────────────────────────────────────────
            Key::Delete => writeb(buf, b"\x1b[3~"),
            Key::Insert => writeb(buf, b"\x1b[2~"),

            // ── Arrows ───────────────────────────────────────────────────
            Key::Up    => self.arrow(buf, b'A', mods),
            Key::Down  => self.arrow(buf, b'B', mods),
            Key::Right => self.arrow(buf, b'C', mods),
            Key::Left  => self.arrow(buf, b'D', mods),

            // ── Home / End ───────────────────────────────────────────────
            Key::Home => {
                if mods.any_active() {
                    csi_mod(buf, b"1", mods, b'H')
                } else if self.app_cursor {
                    writeb(buf, b"\x1bOH")
                } else {
                    writeb(buf, b"\x1b[H")
                }
            }
            Key::End => {
                if mods.any_active() {
                    csi_mod(buf, b"4", mods, b'~')
                } else if self.app_cursor {
                    writeb(buf, b"\x1bOF")
                } else {
                    writeb(buf, b"\x1b[F")
                }
            }

            // ── Page keys ────────────────────────────────────────────────
            Key::PageUp   => {
                if mods.any_active() { csi_mod(buf, b"5", mods, b'~') }
                else { writeb(buf, b"\x1b[5~") }
            }
            Key::PageDown => {
                if mods.any_active() { csi_mod(buf, b"6", mods, b'~') }
                else { writeb(buf, b"\x1b[6~") }
            }

            // ── Function keys ────────────────────────────────────────────
            Key::F(n) => encode_fkey(buf, n, mods)?,

            // ── Keypad cluster ────────────────────────────────────────────
            Key::KpEnter | Key::KpUp | Key::KpDown | Key::KpRight | Key::KpLeft
            | Key::KpHome | Key::KpEnd | Key::KpPageUp | Key::KpPageDown
            | Key::KpBegin | Key::KpInsert | Key::KpDelete | Key::KpChar(_) => {
                return self.encode_keypad(key, buf);
            }

            // Modifier-only keys silently produce nothing in legacy mode.
            Key::Shift | Key::Ctrl | Key::Alt | Key::Super
            | Key::CapsLock | Key::NumLock | Key::ScrollLock | Key::Menu => return None,
        })
    }

    /// Encode the numeric-keypad keys, honouring application-keypad mode.
    fn encode_keypad(&self, key: Key, buf: &mut [u8]) -> Option<usize> {
        let app = self.app_keypad;
        Some(match key {
            Key::KpEnter => if app { writeb(buf, b"\x1bOM") } else { write1(buf, b'\r') },
            Key::KpUp    => self.kp_arrow(buf, b'A'),
            Key::KpDown  => self.kp_arrow(buf, b'B'),
            Key::KpRight => self.kp_arrow(buf, b'C'),
            Key::KpLeft  => self.kp_arrow(buf, b'D'),
            Key::KpHome     => if app { writeb(buf, b"\x1bOw") } else { writeb(buf, b"\x1b[H") },
            Key::KpEnd      => if app { writeb(buf, b"\x1bOq") } else { writeb(buf, b"\x1b[F") },
            Key::KpPageUp   => if app { writeb(buf, b"\x1bOy") } else { writeb(buf, b"\x1b[5~") },
            Key::KpPageDown => if app { writeb(buf, b"\x1bOs") } else { writeb(buf, b"\x1b[6~") },
            Key::KpBegin    => if app { writeb(buf, b"\x1bOu") } else { writeb(buf, b"\x1b[E") },
            Key::KpInsert   => if app { writeb(buf, b"\x1bOp") } else { writeb(buf, b"\x1b[2~") },
            Key::KpDelete   => if app { writeb(buf, b"\x1bOn") } else { writeb(buf, b"\x1b[3~") },
            Key::KpChar(c) => {
                if app {
                    let code: u8 = match c {
                        '0' => b'p', '1' => b'q', '2' => b'r', '3' => b's',
                        '4' => b't', '5' => b'u', '6' => b'v', '7' => b'w',
                        '8' => b'x', '9' => b'y', '.' => b'n', '+' => b'l',
                        '-' => b'm', '*' => b'j', '/' => b'o',
                        _   => return None,
                    };
                    buf[0] = b'\x1b';
                    buf[1] = b'O';
                    buf[2] = code;
                    3
                } else {
                    let mut tmp = [0u8; 4];
                    let n = c.encode_utf8(&mut tmp).len();
                    buf[..n].copy_from_slice(&tmp[..n]);
                    n
                }
            }
            _ => return None,
        })
    }

    /// Arrow key — respects app-cursor mode and modifier CSI.
    fn arrow(&self, buf: &mut [u8], letter: u8, mods: Modifiers) -> usize {
        if mods.any_active() {
            // `CSI 1 ; <mod> A/B/C/D`
            csi_mod(buf, b"1", mods, letter)
        } else {
            // `SS3 A/B/C/D` in app-cursor mode, else `CSI A/B/C/D`.
            let intro = if self.app_cursor { b'O' } else { b'[' };
            buf[0] = b'\x1b';
            buf[1] = intro;
            buf[2] = letter;
            3
        }
    }

    fn kp_arrow(&self, buf: &mut [u8], letter: u8) -> usize {
        // App-keypad and app-cursor both select SS3; default is CSI.
        let intro = if self.app_keypad || self.app_cursor { b'O' } else { b'[' };
        buf[0] = b'\x1b';
        buf[1] = intro;
        buf[2] = letter;
        3
    }

    // ── xterm modifyOtherKeys ─────────────────────────────────────────────────

    fn encode_xterm_modify(&self, key: Key, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        // For plain unmodified keys fall back to legacy.
        if !mods.any_active() {
            return self.encode_legacy(key, mods, buf);
        }
        // Modified printable / special keys → `CSI 27 ; <mod> ; <codepoint> ~`
        let codepoint: u32 = match key {
            Key::Char(c) => c as u32,
            Key::Enter     => 13,
            Key::Backspace => 127,
            Key::Tab       => 9,
            Key::Esc       => 27,
            _ => return self.encode_legacy(key, mods, buf),
        };
        let mod_param = mods.kitty_param().unwrap_or(1);
        Some(write_fmt_csi(buf, 27, mod_param, codepoint))
    }

    // ── Kitty protocol ────────────────────────────────────────────────────────

    #[allow(clippy::unused_self)] // method form keeps the encoder API uniform
    fn encode_kitty(&self, key: Key, state: KeyState, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        // Kitty: `CSI <num> ; <modifier> : <event_type> <terminator>`
        // event_type: 1=press 2=repeat 3=release.
        let mod_param = mods.kitty_param().unwrap_or(1);
        let event_type: u8 = match state {
            KeyState::Pressed  => 1,
            KeyState::Repeated => 2,
            KeyState::Released => 3,
        };
        let (number, terminator) = match kitty_key(key)? {
            KittyKey::Unicode(cp) => (cp, b'u'),
            KittyKey::Tilde(n)    => (n, b'~'),
            KittyKey::Letter(l)   => (1, l),
        };
        Some(write_kitty(buf, number, mod_param, event_type, terminator))
    }

    // ── Mouse encoding ────────────────────────────────────────────────────────

    fn encode_mouse_x10(&self, ev: MouseEvent, buf: &mut [u8]) -> usize {
        // X10: `CSI M <btn+32> <col+32> <row+32>`
        // Normal/ButtonMotion/AnyEvent all use same encoding, differing only
        // in release: button code = 3.
        let mut btn = ev.button.x10_code();
        if ev.mods.shift() { btn |= 0x04; }
        if ev.mods.alt()   { btn |= 0x08; }
        if ev.mods.ctrl()  { btn |= 0x10; }
        if matches!(ev.state, ButtonState::Released)
            && !matches!(self.mouse_mode, MouseMode::X10)
        {
            btn = 3;
        }
        // Clamp to 223 (max encodable in 1 byte + 32).
        let col = (ev.col + 1).min(223) as u8;
        let row = (ev.row + 1).min(223) as u8;
        buf[0] = b'\x1b';
        buf[1] = b'[';
        buf[2] = b'M';
        buf[3] = btn.saturating_add(32);
        buf[4] = col.saturating_add(32);
        buf[5] = row.saturating_add(32);
        6
    }

}

/// SGR mouse encoding (`CSI < btn ; col ; row M/m`) — independent of encoder state.
fn encode_mouse_sgr(ev: MouseEvent, buf: &mut [u8]) -> usize {
    let mut btn = u16::from(ev.button.x10_code());
    if ev.mods.shift() { btn |= 0x04; }
    if ev.mods.alt()   { btn |= 0x08; }
    if ev.mods.ctrl()  { btn |= 0x10; }
    let final_byte: u8 = match ev.state {
        ButtonState::Pressed  => b'M',
        ButtonState::Released => b'm',
    };
    write_sgr_mouse(buf, btn, ev.col + 1, ev.row + 1, final_byte)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Ctrl+letter → C0 byte (a=1 … z=26, space=0, @=0, [=27, \=28, ]=29, ^=30, _=31).
const fn ctrl_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        // `@`/space and the digit aliases collapse to the same C0 codes
        // (Ctrl+2 = NUL, Ctrl+3 = ESC, … Ctrl+7 = US, Ctrl+8 = DEL).
        '@' | ' ' | '2' => Some(0),
        '[' | '3'       => Some(27),
        '\\' | '4'      => Some(28),
        ']' | '5'       => Some(29),
        '^' | '6'       => Some(30),
        '_' | '7'       => Some(31),
        '8'             => Some(127),
        _               => None,
    }
}

/// Encode a character respecting shift and caps-lock.
const fn encode_char(c: char, shift: bool, caps: bool) -> &'static str {
    // For the ASCII range we pre-compute via lookup; fallback is just the char.
    // We return a &'static str from a small table to avoid heap allocation.
    if shift || caps {
        shifted_char(c)
    } else {
        unshifted_char(c)
    }
}

const fn unshifted_char(c: char) -> &'static str {
    // Most chars are their own unshifted form; we just need them as &str.
    // For ASCII printable we return static slices.
    match c {
        'a' => "a", 'b' => "b", 'c' => "c", 'd' => "d", 'e' => "e",
        'f' => "f", 'g' => "g", 'h' => "h", 'i' => "i", 'j' => "j",
        'k' => "k", 'l' => "l", 'm' => "m", 'n' => "n", 'o' => "o",
        'p' => "p", 'q' => "q", 'r' => "r", 's' => "s", 't' => "t",
        'u' => "u", 'v' => "v", 'w' => "w", 'x' => "x", 'y' => "y",
        'z' => "z",
        '1' => "1", '2' => "2", '3' => "3", '4' => "4", '5' => "5",
        '6' => "6", '7' => "7", '8' => "8", '9' => "9", '0' => "0",
        '-' => "-", '=' => "=", '[' => "[", ']' => "]", '\\' => "\\",
        ';' => ";", '\'' => "'", '`' => "`", ',' => ",", '.' => ".", '/' => "/",
        // Space and any non-ASCII fall back to a single space; callers should
        // use Key::Char directly for real non-ASCII text.
        _   => " ",
    }
}

const fn shifted_char(c: char) -> &'static str {
    match c {
        'a' => "A", 'b' => "B", 'c' => "C", 'd' => "D", 'e' => "E",
        'f' => "F", 'g' => "G", 'h' => "H", 'i' => "I", 'j' => "J",
        'k' => "K", 'l' => "L", 'm' => "M", 'n' => "N", 'o' => "O",
        'p' => "P", 'q' => "Q", 'r' => "R", 's' => "S", 't' => "T",
        'u' => "U", 'v' => "V", 'w' => "W", 'x' => "X", 'y' => "Y",
        'z' => "Z",
        '1' => "!", '2' => "@", '3' => "#", '4' => "$", '5' => "%",
        '6' => "^", '7' => "&", '8' => "*", '9' => "(", '0' => ")",
        '-' => "_", '=' => "+", '[' => "{", ']' => "}", '\\' => "|",
        ';' => ":", '\'' => "\"", '`' => "~", ',' => "<", '.' => ">", '/' => "?",
        _   => " ",
    }
}

/// How a key is encoded in the kitty protocol.
///
/// Per the kitty spec, functional keys that have a legacy representation keep
/// using it (number + `~`, or `1` + a letter), while keys with no legacy form
/// use a Private-Use-Area codepoint terminated by `u`.
enum KittyKey {
    /// `CSI <num> ; <mod> : <evt> u` — codepoint keys (text + PUA functionals).
    Unicode(u32),
    /// `CSI <num> ; <mod> : <evt> ~` — tilde-form functionals (Insert, PgUp…).
    Tilde(u32),
    /// `CSI 1 ; <mod> : <evt> <letter>` — letter-form functionals (arrows…).
    Letter(u8),
}

/// Classify a key for kitty encoding.  Returns `None` for keys with no mapping.
const fn kitty_key(key: Key) -> Option<KittyKey> {
    use KittyKey::{Letter, Tilde, Unicode};
    Some(match key {
        Key::Char(c)   => Unicode(c as u32),
        Key::Enter     => Unicode(13),
        Key::Backspace => Unicode(127),
        Key::Tab       => Unicode(9),
        Key::Esc       => Unicode(27),
        // Legacy tilde-form functional keys.
        Key::Insert    => Tilde(2),
        Key::Delete    => Tilde(3),
        Key::PageUp    => Tilde(5),
        Key::PageDown  => Tilde(6),
        // Legacy letter-form functional keys.
        Key::Up        => Letter(b'A'),
        Key::Down      => Letter(b'B'),
        Key::Right     => Letter(b'C'),
        Key::Left      => Letter(b'D'),
        Key::Home      => Letter(b'H'),
        Key::End       => Letter(b'F'),
        // F1–F4 use letter forms; F5+ use tilde forms with their legacy numbers.
        Key::F(1)      => Letter(b'P'),
        Key::F(2)      => Letter(b'Q'),
        Key::F(3)      => Tilde(13),
        Key::F(4)      => Letter(b'S'),
        Key::F(5)      => Tilde(15),
        Key::F(6)      => Tilde(17),
        Key::F(7)      => Tilde(18),
        Key::F(8)      => Tilde(19),
        Key::F(9)      => Tilde(20),
        Key::F(10)     => Tilde(21),
        Key::F(11)     => Tilde(23),
        Key::F(12)     => Tilde(24),
        // Keys with no legacy form — canonical PUA codepoints (kitty spec).
        Key::KpEnter   => Unicode(57414),
        Key::CapsLock  => Unicode(57358),
        Key::ScrollLock => Unicode(57359),
        Key::NumLock   => Unicode(57360),
        Key::Menu      => Unicode(57363),
        Key::Shift     => Unicode(57441),
        Key::Ctrl      => Unicode(57442),
        Key::Alt       => Unicode(57443),
        Key::Super     => Unicode(57444),
        _              => return None,
    })
}

/// Encode a function key sequence, with modifier support.
fn encode_fkey(buf: &mut [u8], n: u8, mods: Modifiers) -> Option<usize> {
    // F1–F4 without mods → SS3 P/Q/R/S; with mods → CSI 1 ; <mod> P/Q/R/S
    // F5–F12 → CSI tilde with numeric codes; with mods → CSI num ; mod ~
    Some(match n {
        1..=4 => {
            let letter = b'P' + n - 1;
            if mods.any_active() {
                csi_mod(buf, b"1", mods, letter)
            } else {
                buf[0] = b'\x1b'; buf[1] = b'O'; buf[2] = letter;
                3
            }
        }
        5 => fkey_tilde(buf, b"15", mods),
        6 => fkey_tilde(buf, b"17", mods),
        7 => fkey_tilde(buf, b"18", mods),
        8 => fkey_tilde(buf, b"19", mods),
        9 => fkey_tilde(buf, b"20", mods),
        10 => fkey_tilde(buf, b"21", mods),
        11 => fkey_tilde(buf, b"23", mods),
        12 => fkey_tilde(buf, b"24", mods),
        _ => return None,
    })
}

/// `CSI <num> [; <mod>] ~`
fn fkey_tilde(buf: &mut [u8], num: &[u8], mods: Modifiers) -> usize {
    let mut n = 0;
    buf[n] = b'\x1b'; n += 1;
    buf[n] = b'[';     n += 1;
    buf[n..n + num.len()].copy_from_slice(num); n += num.len();
    if mods.any_active() {
        let mod_param = mods.kitty_param().unwrap_or(1);
        buf[n] = b';'; n += 1;
        n += write_u8_dec(&mut buf[n..], mod_param);
    }
    buf[n] = b'~'; n += 1;
    n
}

/// `CSI <ps> ; <mod> <final>` — for arrows, Home, End.
fn csi_mod(buf: &mut [u8], ps: &[u8], mods: Modifiers, final_byte: u8) -> usize {
    let mod_param = mods.kitty_param().unwrap_or(1);
    let mut n = 0;
    buf[n] = b'\x1b'; n += 1;
    buf[n] = b'[';     n += 1;
    buf[n..n + ps.len()].copy_from_slice(ps); n += ps.len();
    buf[n] = b';';     n += 1;
    n += write_u8_dec(&mut buf[n..], mod_param);
    buf[n] = final_byte; n += 1;
    n
}

/// `CSI 27 ; <mod> ; <codepoint> ~`
fn write_fmt_csi(buf: &mut [u8], _ps1: u32, mod_param: u8, codepoint: u32) -> usize {
    let mut n = 0;
    buf[n] = b'\x1b'; n += 1;
    buf[n] = b'[';     n += 1;
    buf[n] = b'2'; buf[n+1] = b'7'; n += 2;
    buf[n] = b';'; n += 1;
    n += write_u8_dec(&mut buf[n..], mod_param);
    buf[n] = b';'; n += 1;
    n += write_u32_dec(&mut buf[n..], codepoint);
    buf[n] = b'~'; n += 1;
    n
}

/// Kitty: `CSI <number> ; <mod> : <event> <terminator>`
fn write_kitty(buf: &mut [u8], number: u32, mod_param: u8, event_type: u8, terminator: u8) -> usize {
    let mut n = 0;
    buf[n] = b'\x1b'; n += 1;
    buf[n] = b'[';     n += 1;
    n += write_u32_dec(&mut buf[n..], number);
    buf[n] = b';'; n += 1;
    n += write_u8_dec(&mut buf[n..], mod_param);
    buf[n] = b':'; n += 1;
    n += write_u8_dec(&mut buf[n..], event_type);
    buf[n] = terminator; n += 1;
    n
}

/// SGR mouse: `CSI < <btn> ; <col> ; <row> M/m`
fn write_sgr_mouse(buf: &mut [u8], btn: u16, col: u16, row: u16, final_byte: u8) -> usize {
    let mut n = 0;
    buf[n] = b'\x1b'; n += 1;
    buf[n] = b'[';     n += 1;
    buf[n] = b'<';     n += 1;
    n += write_u16_dec(&mut buf[n..], btn);
    buf[n] = b';'; n += 1;
    n += write_u16_dec(&mut buf[n..], col);
    buf[n] = b';'; n += 1;
    n += write_u16_dec(&mut buf[n..], row);
    buf[n] = final_byte; n += 1;
    n
}

fn write1(buf: &mut [u8], b: u8) -> usize {
    buf[0] = b;
    1
}

fn writeb(buf: &mut [u8], s: &[u8]) -> usize {
    buf[..s.len()].copy_from_slice(s);
    s.len()
}

/// Write a `u8` as decimal digits, returns number of bytes written.
fn write_u8_dec(buf: &mut [u8], mut v: u8) -> usize {
    if v == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 3];
    let mut len = 0;
    while v > 0 { tmp[len] = b'0' + v % 10; len += 1; v /= 10; }
    for i in 0..len { buf[i] = tmp[len - 1 - i]; }
    len
}

/// Write a `u16` as decimal digits.
fn write_u16_dec(buf: &mut [u8], mut v: u16) -> usize {
    if v == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 5];
    let mut len = 0;
    while v > 0 { tmp[len] = b'0' + (v % 10) as u8; len += 1; v /= 10; }
    for i in 0..len { buf[i] = tmp[len - 1 - i]; }
    len
}

/// Write a `u32` as decimal digits.
fn write_u32_dec(buf: &mut [u8], mut v: u32) -> usize {
    if v == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 10];
    let mut len = 0;
    while v > 0 { tmp[len] = b'0' + (v % 10) as u8; len += 1; v /= 10; }
    for i in 0..len { buf[i] = tmp[len - 1 - i]; }
    len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{Key, KeyState, Modifiers};
    use crate::mouse::{MouseButton, MouseEvent, ButtonState};

    fn enc() -> Encoder { Encoder::new() }
    fn buf() -> [u8; 64] { [0u8; 64] }

    fn encode(enc: &Encoder, key: Key, mods: Modifiers) -> Vec<u8> {
        let mut b = buf();
        let n = enc.encode_key(key, KeyState::Pressed, mods, &mut b).unwrap_or(0);
        b[..n].to_vec()
    }

    #[test]
    fn plain_letters() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('a'), Modifiers::NONE), b"a");
        assert_eq!(encode(&e, Key::Char('z'), Modifiers::NONE), b"z");
        assert_eq!(encode(&e, Key::Char('a'), Modifiers::SHIFT), b"A");
    }

    #[test]
    fn ctrl_letters() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('c'), Modifiers::CTRL), b"\x03");
        assert_eq!(encode(&e, Key::Char('a'), Modifiers::CTRL), b"\x01");
        assert_eq!(encode(&e, Key::Char('z'), Modifiers::CTRL), b"\x1a");
    }

    #[test]
    fn arrows_normal() {
        let e = enc();
        assert_eq!(encode(&e, Key::Up,    Modifiers::NONE), b"\x1b[A");
        assert_eq!(encode(&e, Key::Down,  Modifiers::NONE), b"\x1b[B");
        assert_eq!(encode(&e, Key::Right, Modifiers::NONE), b"\x1b[C");
        assert_eq!(encode(&e, Key::Left,  Modifiers::NONE), b"\x1b[D");
    }

    #[test]
    fn arrows_app_cursor() {
        let mut e = enc();
        e.app_cursor = true;
        assert_eq!(encode(&e, Key::Up,    Modifiers::NONE), b"\x1bOA");
        assert_eq!(encode(&e, Key::Down,  Modifiers::NONE), b"\x1bOB");
        assert_eq!(encode(&e, Key::Right, Modifiers::NONE), b"\x1bOC");
        assert_eq!(encode(&e, Key::Left,  Modifiers::NONE), b"\x1bOD");
    }

    #[test]
    fn arrows_modified() {
        let e = enc();
        assert_eq!(encode(&e, Key::Up, Modifiers::SHIFT),   b"\x1b[1;2A");
        assert_eq!(encode(&e, Key::Up, Modifiers::CTRL),    b"\x1b[1;5A");
        assert_eq!(encode(&e, Key::Up, Modifiers::SHIFT | Modifiers::CTRL), b"\x1b[1;6A");
    }

    #[test]
    fn fkeys_no_mods() {
        let e = enc();
        assert_eq!(encode(&e, Key::F(1),  Modifiers::NONE), b"\x1bOP");
        assert_eq!(encode(&e, Key::F(2),  Modifiers::NONE), b"\x1bOQ");
        assert_eq!(encode(&e, Key::F(5),  Modifiers::NONE), b"\x1b[15~");
        assert_eq!(encode(&e, Key::F(12), Modifiers::NONE), b"\x1b[24~");
    }

    #[test]
    fn fkeys_with_mods() {
        let e = enc();
        assert_eq!(encode(&e, Key::F(1), Modifiers::SHIFT), b"\x1b[1;2P");
        assert_eq!(encode(&e, Key::F(5), Modifiers::CTRL),  b"\x1b[15;5~");
    }

    #[test]
    fn control_chars() {
        let e = enc();
        assert_eq!(encode(&e, Key::Enter,     Modifiers::NONE), b"\r");
        assert_eq!(encode(&e, Key::Backspace, Modifiers::NONE), b"\x7f");
        assert_eq!(encode(&e, Key::Tab,       Modifiers::NONE), b"\t");
        assert_eq!(encode(&e, Key::Tab,       Modifiers::SHIFT), b"\x1b[Z");
        assert_eq!(encode(&e, Key::Esc,       Modifiers::NONE), b"\x1b");
        assert_eq!(encode(&e, Key::Delete,    Modifiers::NONE), b"\x1b[3~");
    }

    #[test]
    fn mouse_sgr_press() {
        let mut e = enc();
        e.mouse_mode = MouseMode::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            state:  ButtonState::Pressed,
            col:    4,
            row:    2,
            mods:   Modifiers::NONE,
        };
        let mut b = buf();
        let n = e.encode_mouse(ev, &mut b).unwrap();
        assert_eq!(&b[..n], b"\x1b[<0;5;3M");
    }

    #[test]
    fn mouse_sgr_release() {
        let mut e = enc();
        e.mouse_mode = MouseMode::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            state:  ButtonState::Released,
            col:    0,
            row:    0,
            mods:   Modifiers::NONE,
        };
        let mut b = buf();
        let n = e.encode_mouse(ev, &mut b).unwrap();
        assert_eq!(&b[..n], b"\x1b[<0;1;1m");
    }

    #[test]
    fn kitty_mode_plain() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        let mut b = buf();
        let n = e.encode_key(Key::Char('a'), KeyState::Pressed, Modifiers::NONE, &mut b).unwrap();
        // CSI 97 ; 1 : 1 u
        assert_eq!(&b[..n], b"\x1b[97;1:1u");
    }

    #[test]
    fn kitty_functional_keys_use_correct_terminators() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // Letter-form functionals: CSI 1 ; <mod> : <evt> <letter>
        assert_eq!(encode(&e, Key::Up,   Modifiers::NONE), b"\x1b[1;1:1A");
        assert_eq!(encode(&e, Key::Home, Modifiers::NONE), b"\x1b[1;1:1H");
        // Tilde-form functionals keep their legacy numbers (no PUA aliasing).
        assert_eq!(encode(&e, Key::Insert,   Modifiers::NONE), b"\x1b[2;1:1~");
        assert_eq!(encode(&e, Key::Delete,   Modifiers::NONE), b"\x1b[3;1:1~");
        assert_eq!(encode(&e, Key::PageUp,   Modifiers::NONE), b"\x1b[5;1:1~");
        assert_eq!(encode(&e, Key::PageDown, Modifiers::NONE), b"\x1b[6;1:1~");
        // PUA-only keys keep the `u` terminator with their canonical codepoints.
        assert_eq!(encode(&e, Key::CapsLock,   Modifiers::NONE), b"\x1b[57358;1:1u");
        assert_eq!(encode(&e, Key::ScrollLock, Modifiers::NONE), b"\x1b[57359;1:1u");
        assert_eq!(encode(&e, Key::NumLock,    Modifiers::NONE), b"\x1b[57360;1:1u");
    }

    #[test]
    fn kitty_mode_release() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        let mut b = buf();
        let n = e.encode_key(Key::Char('a'), KeyState::Released, Modifiers::NONE, &mut b).unwrap();
        assert_eq!(&b[..n], b"\x1b[97;1:3u");
    }

    #[test]
    fn modifier_only_produces_nothing_in_legacy() {
        let e = enc();
        let mut b = buf();
        let r = e.encode_key(Key::Shift, KeyState::Pressed, Modifiers::NONE, &mut b);
        assert!(r.is_none());
    }

    #[test]
    fn alt_letter() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('a'), Modifiers::ALT), b"\x1ba");
    }

    #[test]
    fn number_row_shifted() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('1'), Modifiers::SHIFT), b"!");
        assert_eq!(encode(&e, Key::Char('2'), Modifiers::SHIFT), b"@");
        assert_eq!(encode(&e, Key::Char('6'), Modifiers::SHIFT), b"^");
    }
}
