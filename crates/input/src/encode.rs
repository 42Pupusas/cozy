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

// ── Output cursor ────────────────────────────────────────────────────────────

/// A write cursor over a caller-supplied `&mut [u8]`.
///
/// Every VT sequence builder in this module used to be a free function
/// threading `buf: &mut [u8]` plus a hand-rolled running offset (`n`, `n +=
/// ...`) through every call — the exact shape a small stateful writer exists
/// to replace. `SeqWriter` owns that offset once; every write is a method
/// that advances it, and [`SeqWriter::len`] replaces the old "return the
/// final `n`" convention.
struct SeqWriter<'a> {
    buf: &'a mut [u8],
    n:   usize,
}

impl<'a> SeqWriter<'a> {
    const fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, n: 0 }
    }

    /// Bytes written so far — the return value every old free function
    /// produced by hand.
    const fn len(&self) -> usize {
        self.n
    }

    fn byte(&mut self, b: u8) -> &mut Self {
        self.buf[self.n] = b;
        self.n += 1;
        self
    }

    fn bytes(&mut self, s: &[u8]) -> &mut Self {
        self.buf[self.n..self.n + s.len()].copy_from_slice(s);
        self.n += s.len();
        self
    }

    /// `ESC` — used to prefix almost every escape sequence below.
    fn esc(&mut self) -> &mut Self {
        self.byte(b'\x1b')
    }

    /// `ESC [` — CSI, the common introducer for cursor/functional keys.
    fn csi(&mut self) -> &mut Self {
        self.esc().byte(b'[')
    }

    /// Decimal digits of `v`, no leading zeros (`0` itself writes `"0"`).
    fn dec(&mut self, mut v: u32) -> &mut Self {
        if v == 0 {
            return self.byte(b'0');
        }
        let mut tmp = [0u8; 10];
        let mut len = 0;
        while v > 0 {
            tmp[len] = b'0' + (v % 10) as u8;
            len += 1;
            v /= 10;
        }
        for i in 0..len {
            self.byte(tmp[len - 1 - i]);
        }
        self
    }
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
            MouseMode::Sgr => Some(Self::encode_mouse_sgr(ev, buf)),
            _ => Some(self.encode_mouse_x10(ev, buf)),
        }
    }

    // ── Legacy encoding ───────────────────────────────────────────────────────

    fn encode_legacy(&self, key: Key, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        // Ctrl+letter → C0 control codes.
        if mods.ctrl()
            && let Key::Char(c) = key
            && let Some(byte) = ctrl_byte(c)
        {
            let mut w = SeqWriter::new(buf);
            w.byte(byte);
            return Some(w.len());
        }

        Some(match key {
            // ── Printable characters ──────────────────────────────────────
            Key::Char(c) => {
                let mut w = SeqWriter::new(buf);
                // Alt prefixes with ESC.
                if mods.alt() {
                    w.esc();
                }
                w.bytes(encode_char(c, mods.shift(), mods.caps()).as_bytes());
                w.len()
            }

            // ── Enter / Backspace / Tab / Esc ────────────────────────────
            Key::Enter     => { let mut w = SeqWriter::new(buf); w.byte(b'\r'); w.len() }
            Key::Backspace => { let mut w = SeqWriter::new(buf); w.byte(b'\x7f'); w.len() }
            Key::Tab       => {
                let mut w = SeqWriter::new(buf);
                if mods.shift() {
                    w.bytes(b"\x1b[Z");
                } else {
                    w.byte(b'\t');
                }
                w.len()
            }
            Key::Esc       => { let mut w = SeqWriter::new(buf); w.esc(); w.len() }

            // ── Delete / Insert ──────────────────────────────────────────
            Key::Delete => { let mut w = SeqWriter::new(buf); w.bytes(b"\x1b[3~"); w.len() }
            Key::Insert => { let mut w = SeqWriter::new(buf); w.bytes(b"\x1b[2~"); w.len() }

            // ── Arrows ───────────────────────────────────────────────────
            Key::Up    => self.arrow(buf, b'A', mods),
            Key::Down  => self.arrow(buf, b'B', mods),
            Key::Right => self.arrow(buf, b'C', mods),
            Key::Left  => self.arrow(buf, b'D', mods),

            // ── Home / End ───────────────────────────────────────────────
            Key::Home => {
                let mut w = SeqWriter::new(buf);
                if mods.any_active() {
                    w.csi_mod(b"1", mods, b'H');
                } else if self.app_cursor {
                    w.bytes(b"\x1bOH");
                } else {
                    w.bytes(b"\x1b[H");
                }
                w.len()
            }
            Key::End => {
                let mut w = SeqWriter::new(buf);
                if mods.any_active() {
                    w.csi_mod(b"4", mods, b'~');
                } else if self.app_cursor {
                    w.bytes(b"\x1bOF");
                } else {
                    w.bytes(b"\x1b[F");
                }
                w.len()
            }

            // ── Page keys ────────────────────────────────────────────────
            Key::PageUp   => {
                let mut w = SeqWriter::new(buf);
                if mods.any_active() { w.csi_mod(b"5", mods, b'~'); } else { w.bytes(b"\x1b[5~"); }
                w.len()
            }
            Key::PageDown => {
                let mut w = SeqWriter::new(buf);
                if mods.any_active() { w.csi_mod(b"6", mods, b'~'); } else { w.bytes(b"\x1b[6~"); }
                w.len()
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
            Key::KpEnter => {
                let mut w = SeqWriter::new(buf);
                if app { w.bytes(b"\x1bOM"); } else { w.byte(b'\r'); }
                w.len()
            }
            Key::KpUp    => self.kp_arrow(buf, b'A'),
            Key::KpDown  => self.kp_arrow(buf, b'B'),
            Key::KpRight => self.kp_arrow(buf, b'C'),
            Key::KpLeft  => self.kp_arrow(buf, b'D'),
            Key::KpHome     => Self::kp_or(buf, app, b"\x1bOw", b"\x1b[H"),
            Key::KpEnd      => Self::kp_or(buf, app, b"\x1bOq", b"\x1b[F"),
            Key::KpPageUp   => Self::kp_or(buf, app, b"\x1bOy", b"\x1b[5~"),
            Key::KpPageDown => Self::kp_or(buf, app, b"\x1bOs", b"\x1b[6~"),
            Key::KpBegin    => Self::kp_or(buf, app, b"\x1bOu", b"\x1b[E"),
            Key::KpInsert   => Self::kp_or(buf, app, b"\x1bOp", b"\x1b[2~"),
            Key::KpDelete   => Self::kp_or(buf, app, b"\x1bOn", b"\x1b[3~"),
            Key::KpChar(c) => {
                let mut w = SeqWriter::new(buf);
                if app {
                    let code: u8 = match c {
                        '0' => b'p', '1' => b'q', '2' => b'r', '3' => b's',
                        '4' => b't', '5' => b'u', '6' => b'v', '7' => b'w',
                        '8' => b'x', '9' => b'y', '.' => b'n', '+' => b'l',
                        '-' => b'm', '*' => b'j', '/' => b'o',
                        _   => return None,
                    };
                    w.esc().byte(b'O').byte(code);
                } else {
                    let mut tmp = [0u8; 4];
                    let src = c.encode_utf8(&mut tmp).as_bytes();
                    w.bytes(src);
                }
                w.len()
            }
            _ => return None,
        })
    }

    /// Write `app_bytes` if `app` else `plain_bytes`; shared by the keypad's
    /// many "app-keypad selects one literal sequence, else another" arms.
    fn kp_or(buf: &mut [u8], app: bool, app_bytes: &[u8], plain_bytes: &[u8]) -> usize {
        let mut w = SeqWriter::new(buf);
        w.bytes(if app { app_bytes } else { plain_bytes });
        w.len()
    }

    /// Arrow key — respects app-cursor mode and modifier CSI.
    fn arrow(&self, buf: &mut [u8], letter: u8, mods: Modifiers) -> usize {
        let mut w = SeqWriter::new(buf);
        if mods.any_active() {
            // `CSI 1 ; <mod> A/B/C/D`
            w.csi_mod(b"1", mods, letter);
        } else {
            // `SS3 A/B/C/D` in app-cursor mode, else `CSI A/B/C/D`.
            let intro = if self.app_cursor { b'O' } else { b'[' };
            w.esc().byte(intro).byte(letter);
        }
        w.len()
    }

    fn kp_arrow(&self, buf: &mut [u8], letter: u8) -> usize {
        // App-keypad and app-cursor both select SS3; default is CSI.
        let intro = if self.app_keypad || self.app_cursor { b'O' } else { b'[' };
        let mut w = SeqWriter::new(buf);
        w.esc().byte(intro).byte(letter);
        w.len()
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
        let mut w = SeqWriter::new(buf);
        w.csi().bytes(b"27").byte(b';').dec(u32::from(mod_param)).byte(b';').dec(codepoint).byte(b'~');
        Some(w.len())
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
        let mut w = SeqWriter::new(buf);
        w.csi().dec(number).byte(b';').dec(u32::from(mod_param)).byte(b':').dec(u32::from(event_type)).byte(terminator);
        Some(w.len())
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
        let mut w = SeqWriter::new(buf);
        w.csi().byte(b'M')
            .byte(btn.saturating_add(32))
            .byte(col.saturating_add(32))
            .byte(row.saturating_add(32));
        w.len()
    }

    /// SGR mouse encoding (`CSI < btn ; col ; row M/m`) — independent of
    /// encoder state, so it takes no `&self`.
    fn encode_mouse_sgr(ev: MouseEvent, buf: &mut [u8]) -> usize {
        let mut btn = u16::from(ev.button.x10_code());
        if ev.mods.shift() { btn |= 0x04; }
        if ev.mods.alt()   { btn |= 0x08; }
        if ev.mods.ctrl()  { btn |= 0x10; }
        let final_byte: u8 = match ev.state {
            ButtonState::Pressed  => b'M',
            ButtonState::Released => b'm',
        };
        let mut w = SeqWriter::new(buf);
        w.csi().byte(b'<')
            .dec(u32::from(btn)).byte(b';')
            .dec(u32::from(ev.col + 1)).byte(b';')
            .dec(u32::from(ev.row + 1)).byte(final_byte);
        w.len()
    }
}

// ── SeqWriter: sequence-shaped helpers ──────────────────────────────────────
//
// These build on the primitive `byte`/`bytes`/`esc`/`csi`/`dec` methods above
// and encode a specific VT sequence shape shared by several keys.
impl SeqWriter<'_> {
    /// `CSI <ps> ; <mod> <final>` — for arrows, Home, End, page keys.
    fn csi_mod(&mut self, ps: &[u8], mods: Modifiers, final_byte: u8) -> &mut Self {
        let mod_param = mods.kitty_param().unwrap_or(1);
        self.csi().bytes(ps).byte(b';').dec(u32::from(mod_param)).byte(final_byte)
    }

    /// `CSI <num> [; <mod>] ~`
    fn fkey_tilde(&mut self, num: &[u8], mods: Modifiers) -> &mut Self {
        self.csi().bytes(num);
        if mods.any_active() {
            let mod_param = mods.kitty_param().unwrap_or(1);
            self.byte(b';').dec(u32::from(mod_param));
        }
        self.byte(b'~')
    }
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
    let mut w = SeqWriter::new(buf);
    match n {
        1..=4 => {
            let letter = b'P' + n - 1;
            if mods.any_active() {
                w.csi_mod(b"1", mods, letter);
            } else {
                w.esc().byte(b'O').byte(letter);
            }
        }
        5  => { w.fkey_tilde(b"15", mods); }
        6  => { w.fkey_tilde(b"17", mods); }
        7  => { w.fkey_tilde(b"18", mods); }
        8  => { w.fkey_tilde(b"19", mods); }
        9  => { w.fkey_tilde(b"20", mods); }
        10 => { w.fkey_tilde(b"21", mods); }
        11 => { w.fkey_tilde(b"23", mods); }
        12 => { w.fkey_tilde(b"24", mods); }
        _ => return None,
    }
    Some(w.len())
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
