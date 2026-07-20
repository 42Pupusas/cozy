//! VT byte-sequence encoding for keyboard and mouse events.
//!
//! The [`Encoder`] holds the negotiated protocol state (keyboard mode, mouse
//! mode, application-cursor/keypad flags) and produces byte sequences into a
//! caller-supplied `&mut [u8]` buffer.  No heap allocation.

use crate::key::{Key, KeyState, Modifiers};
use crate::mouse::{ButtonState, MouseEvent, MouseMode};

// ── Kitty progressive-enhancement flag bits (`CSI = flags ; mode u`) ──────────
// See <https://sw.kovidgoyal.net/kitty/keyboard-protocol/#progressive-enhancement>.

/// `0b1`: disambiguate escape codes (Esc/alt+key/ctrl+key/... use `CSI u`).
const KITTY_DISAMBIGUATE: u8 = 0b1;
/// `0b10`: report key repeat and release events (not just press).
const KITTY_REPORT_EVENT_TYPES: u8 = 0b10;
/// `0b1000`: report every key, including plain text keys, as `CSI u`.
const KITTY_REPORT_ALL_KEYS: u8 = 0b1000;

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

impl KeyboardMode {
    /// Pack into one byte for cross-thread transport (e.g. an `AtomicU8`
    /// shuttling the grid's negotiated mode to the UI thread's `Encoder`).
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Legacy => 0,
            Self::XtermModifyOtherKeys => 1,
            Self::Kitty => 2,
        }
    }

    /// Inverse of [`Self::to_u8`]. Any unrecognized byte decodes to `Legacy`
    /// (fail safe -- worst case keys are encoded more conservatively than
    /// the app asked for, never less).
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::XtermModifyOtherKeys,
            2 => Self::Kitty,
            _ => Self::Legacy,
        }
    }
}

/// Stateful encoder: holds negotiated protocol flags and produces byte sequences.
#[derive(Clone, Debug, Default)]
pub struct Encoder {
    pub keyboard_mode: KeyboardMode,
    /// Tracking granularity: `Off`/`X10`/`Normal`/`ButtonMotion`/`AnyEvent`.
    /// Setting this directly to `MouseMode::Sgr` is also accepted (kept for
    /// backward compatibility with callers that don't track granularity and
    /// encoding separately) and is treated the same as `Normal` for motion
    /// gating, always SGR-encoded regardless of `sgr_mouse`.
    pub mouse_mode: MouseMode,
    /// SGR coordinate encoding (`CSI ? 1006 h`), layered independently on
    /// top of `mouse_mode`'s tracking granularity -- xterm lets an app
    /// toggle this without changing which events are tracked at all.
    pub sgr_mouse: bool,
    /// Application-cursor mode (`CSI ? 1 h`): arrow keys send `SS3` instead of `CSI`.
    pub app_cursor: bool,
    /// Application-keypad mode (`CSI = h`): keypad sends application sequences.
    pub app_keypad: bool,
    /// Kitty progressive enhancement flags bitmask (only meaningful in Kitty mode).
    pub kitty_flags: u8,
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
    n: usize,
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
        key: Key,
        state: KeyState,
        mods: Modifiers,
        buf: &mut [u8],
    ) -> Option<usize> {
        // Legacy/xterm only emit on press/repeat. Kitty emits *release* events
        // only when the app enabled the "report event types" progressive-
        // enhancement flag (bit 0b10). Without it, a release must not be sent:
        // apps that didn't ask for event types (e.g. fish) re-insert the
        // release as a second press, so every typed key registers twice.
        let report = match self.keyboard_mode {
            KeyboardMode::Kitty => {
                matches!(state, KeyState::Pressed | KeyState::Repeated)
                    || self.kitty_flags & KITTY_REPORT_EVENT_TYPES != 0
            }
            _ => matches!(state, KeyState::Pressed | KeyState::Repeated),
        };
        if !report {
            return None;
        }
        if key.is_modifier_only() && self.keyboard_mode != KeyboardMode::Kitty {
            return None;
        }

        match self.keyboard_mode {
            KeyboardMode::Legacy => self.encode_legacy(key, mods, buf),
            KeyboardMode::XtermModifyOtherKeys => self.encode_xterm_modify(key, mods, buf),
            KeyboardMode::Kitty => self.encode_kitty(key, state, mods, buf),
        }
    }

    /// Encode a mouse event into `buf`.  Returns the number of bytes written,
    /// or `None` if mouse reporting is off, or if this is a motion report
    /// the negotiated tracking mode doesn't ask for (plain `Normal`/`X10`
    /// only report press/release; `ButtonMotion` adds drag; `AnyEvent` adds
    /// bare hover too).
    pub fn encode_mouse(&self, ev: MouseEvent, buf: &mut [u8]) -> Option<usize> {
        if matches!(ev.state, ButtonState::Moved) {
            let wants_motion = match self.mouse_mode {
                MouseMode::AnyEvent => true,
                // ButtonMotion (and Sgr, kept as a granularity alias) only
                // reports motion while some button is held.
                MouseMode::ButtonMotion | MouseMode::Sgr => {
                    !matches!(ev.button, crate::mouse::MouseButton::None)
                }
                MouseMode::Off | MouseMode::X10 | MouseMode::Normal => false,
            };
            if !wants_motion {
                return None;
            }
        } else if matches!(self.mouse_mode, MouseMode::Off) {
            return None;
        }
        if self.sgr_mouse || matches!(self.mouse_mode, MouseMode::Sgr) {
            Some(Self::encode_mouse_sgr(ev, buf))
        } else {
            Some(self.encode_mouse_x10(ev, buf))
        }
    }

    // ── Legacy encoding ───────────────────────────────────────────────────────

    fn encode_legacy(&self, key: Key, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        // Ctrl+letter → C0 control codes.
        if mods.ctrl()
            && let Key::Char(c) = key
            && let Some(byte) = c.ctrl_byte()
        {
            let mut w = SeqWriter::new(buf);
            w.byte(byte);
            return Some(w.len());
        }

        if key.is_legacy_editing_cluster() {
            return self.encode_legacy_editing(key, mods, buf);
        }

        Some(match key {
            // ── Printable characters ──────────────────────────────────────
            Key::Char(c) => {
                let mut w = SeqWriter::new(buf);
                // Alt prefixes with ESC.
                if mods.alt() {
                    w.esc();
                }
                let mut tmp = [0u8; 4];
                let src = c.encode_terminal(mods.shift(), mods.caps(), &mut tmp);
                w.bytes(src);
                w.len()
            }

            // ── Arrows ───────────────────────────────────────────────────
            Key::Up => self.arrow(buf, b'A', mods),
            Key::Down => self.arrow(buf, b'B', mods),
            Key::Right => self.arrow(buf, b'C', mods),
            Key::Left => self.arrow(buf, b'D', mods),

            // ── Function keys ────────────────────────────────────────────
            Key::F(n) => {
                let mut w = SeqWriter::new(buf);
                w.fkey(n, mods)?;
                w.len()
            }

            // ── Keypad cluster ────────────────────────────────────────────
            Key::KpEnter
            | Key::KpUp
            | Key::KpDown
            | Key::KpRight
            | Key::KpLeft
            | Key::KpHome
            | Key::KpEnd
            | Key::KpPageUp
            | Key::KpPageDown
            | Key::KpBegin
            | Key::KpInsert
            | Key::KpDelete
            | Key::KpChar(_) => {
                return self.encode_keypad(key, buf);
            }

            // Modifier-only keys silently produce nothing in legacy mode.
            Key::Shift
            | Key::Ctrl
            | Key::Alt
            | Key::Super
            | Key::CapsLock
            | Key::NumLock
            | Key::ScrollLock
            | Key::Menu => return None,

            // Handled by the `Key::is_legacy_editing_cluster` early-return above.
            Key::Enter
            | Key::Backspace
            | Key::Tab
            | Key::Esc
            | Key::Delete
            | Key::Insert
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => unreachable!("handled by encode_legacy_editing"),
        })
    }

    /// The "editing cluster": Enter/Backspace/Tab/Esc/Delete/Insert/Home/End/
    /// PageUp/PageDown. Split out of [`Self::encode_legacy`] purely to keep
    /// that function under clippy's line-count lint — these keys share no
    /// state or logic with the rest of the legacy dispatch.
    fn encode_legacy_editing(&self, key: Key, mods: Modifiers, buf: &mut [u8]) -> Option<usize> {
        let mut w = SeqWriter::new(buf);
        match key {
            Key::Enter => {
                w.byte(b'\r');
            }
            Key::Backspace => {
                w.byte(b'\x7f');
            }
            Key::Tab => {
                if mods.shift() {
                    w.bytes(b"\x1b[Z");
                } else {
                    w.byte(b'\t');
                }
            }
            Key::Esc => {
                w.esc();
            }
            Key::Delete => {
                w.bytes(b"\x1b[3~");
            }
            Key::Insert => {
                w.bytes(b"\x1b[2~");
            }
            Key::Home => {
                if mods.any_active() {
                    w.csi_mod(b"1", mods, b'H');
                } else if self.app_cursor {
                    w.bytes(b"\x1bOH");
                } else {
                    w.bytes(b"\x1b[H");
                }
            }
            Key::End => {
                if mods.any_active() {
                    w.csi_mod(b"4", mods, b'~');
                } else if self.app_cursor {
                    w.bytes(b"\x1bOF");
                } else {
                    w.bytes(b"\x1b[F");
                }
            }
            Key::PageUp => {
                if mods.any_active() {
                    w.csi_mod(b"5", mods, b'~');
                } else {
                    w.bytes(b"\x1b[5~");
                }
            }
            Key::PageDown => {
                if mods.any_active() {
                    w.csi_mod(b"6", mods, b'~');
                } else {
                    w.bytes(b"\x1b[6~");
                }
            }
            _ => return None,
        }
        Some(w.len())
    }

    /// Encode the numeric-keypad keys, honouring application-keypad mode.
    fn encode_keypad(&self, key: Key, buf: &mut [u8]) -> Option<usize> {
        let app = self.app_keypad;
        Some(match key {
            Key::KpEnter => {
                let mut w = SeqWriter::new(buf);
                if app {
                    w.bytes(b"\x1bOM");
                } else {
                    w.byte(b'\r');
                }
                w.len()
            }
            Key::KpUp => self.kp_arrow(buf, b'A'),
            Key::KpDown => self.kp_arrow(buf, b'B'),
            Key::KpRight => self.kp_arrow(buf, b'C'),
            Key::KpLeft => self.kp_arrow(buf, b'D'),
            Key::KpHome => Self::kp_or(buf, app, b"\x1bOw", b"\x1b[H"),
            Key::KpEnd => Self::kp_or(buf, app, b"\x1bOq", b"\x1b[F"),
            Key::KpPageUp => Self::kp_or(buf, app, b"\x1bOy", b"\x1b[5~"),
            Key::KpPageDown => Self::kp_or(buf, app, b"\x1bOs", b"\x1b[6~"),
            Key::KpBegin => Self::kp_or(buf, app, b"\x1bOu", b"\x1b[E"),
            Key::KpInsert => Self::kp_or(buf, app, b"\x1bOp", b"\x1b[2~"),
            Key::KpDelete => Self::kp_or(buf, app, b"\x1bOn", b"\x1b[3~"),
            Key::KpChar(c) => {
                let mut w = SeqWriter::new(buf);
                if app {
                    let code: u8 = match c {
                        '0' => b'p',
                        '1' => b'q',
                        '2' => b'r',
                        '3' => b's',
                        '4' => b't',
                        '5' => b'u',
                        '6' => b'v',
                        '7' => b'w',
                        '8' => b'x',
                        '9' => b'y',
                        '.' => b'n',
                        '+' => b'l',
                        '-' => b'm',
                        '*' => b'j',
                        '/' => b'o',
                        _ => return None,
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
        let intro = if self.app_keypad || self.app_cursor {
            b'O'
        } else {
            b'['
        };
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
            Key::Enter => 13,
            Key::Backspace => 127,
            Key::Tab => 9,
            Key::Esc => 27,
            _ => return self.encode_legacy(key, mods, buf),
        };
        let mod_param = mods.kitty_param().unwrap_or(1);
        let mut w = SeqWriter::new(buf);
        w.csi()
            .bytes(b"27")
            .byte(b';')
            .dec(u32::from(mod_param))
            .byte(b';')
            .dec(codepoint)
            .byte(b'~');
        Some(w.len())
    }

    // ── Kitty protocol ────────────────────────────────────────────────────────

    fn encode_kitty(
        &self,
        key: Key,
        state: KeyState,
        mods: Modifiers,
        buf: &mut [u8],
    ) -> Option<usize> {
        // Progressive-enhancement flags that change *which* keys get CSI-u
        // treatment at all -- distinct from `KITTY_REPORT_EVENT_TYPES`
        // (already gated in `encode_key`), which only changes whether a
        // release is reported once we've decided to emit CSI-u.
        //
        // Per the kitty spec, without these flags a freshly-pushed Kitty
        // mode (flags == 0, exactly what `fish` leaves behind for anything
        // it execs -- fish itself only ever requests 0b1) must still emit
        // plain legacy bytes for text keys and the functional-key table,
        // *not* `CSI u` escape codes. Skipping this was the actual bug:
        // every character, including bare letters typed into a password
        // prompt, was being wrapped in `CSI <code>;<mod>:<evt>u` -- a raw-
        // mode reader that only understands single bytes (`sudo`/`getpass`,
        // which never negotiates Kitty mode itself) saw that escape code as
        // several bogus keystrokes instead of the one letter that was typed.
        let report_all_keys = self.kitty_flags & KITTY_REPORT_ALL_KEYS != 0;
        let disambiguate = self.kitty_flags & KITTY_DISAMBIGUATE != 0;
        // Text keys only report *release* once "report all keys" is on (a
        // repeat still just resends the same text, exactly as legacy mode's
        // key-repeat already does -- only release has no representation of
        // its own for a key that produces text).
        let is_bare_text = matches!(key, Key::Char(_)) && !mods.ctrl() && !mods.alt();
        if is_bare_text && !report_all_keys && matches!(state, KeyState::Released) {
            return None;
        }
        // The legacy text-key algorithm (alt -> ESC prefix, else ctrl -> C0
        // table, else shift -> shifted glyph, else bare) only ever consults
        // alt/ctrl/shift. Any other modifier active (super/hyper/meta) has
        // no legacy representation at all -- "any other combination... is
        // output as the appropriate CSI u escape code", unconditionally,
        // not gated behind any progressive-enhancement flag.
        let legacy_representable = !mods.sup();
        if !report_all_keys && legacy_representable {
            // Bare text keys and Enter/Tab/Backspace are *always* sent as
            // legacy bytes -- the spec's explicit exception, so `reset`
            // still works after a crashed program leaves Kitty mode enabled
            // -- and every other key falls back to legacy too unless the
            // app asked to disambiguate escape codes.
            let always_legacy = matches!(key, Key::Enter | Key::Backspace | Key::Tab) || is_bare_text;
            if (always_legacy || !disambiguate)
                && let Some(n) = self.encode_legacy(key, mods, buf)
            {
                return Some(n);
            }
            // No legacy representation (PUA-only keys, bare modifiers) --
            // fall through to CSI-u, the spec's catch-all for anything
            // outside the legacy table.
        }
        // Kitty: `CSI <num> ; <modifier> : <event_type> <terminator>`
        // event_type: 1=press 2=repeat 3=release.
        let mod_param = mods.kitty_param().unwrap_or(1);
        let report_events = self.kitty_flags & KITTY_REPORT_EVENT_TYPES != 0;
        let event_type: u8 = match state {
            KeyState::Pressed => 1,
            // Without report-event-types, repeats are indistinguishable from
            // presses (spec: "key repeat events are treated as key press
            // events").
            KeyState::Repeated => {
                if report_events {
                    2
                } else {
                    1
                }
            }
            KeyState::Released => 3,
        };
        let (number, terminator) = match KittyKey::from_key(key)? {
            KittyKey::Unicode(cp) => (cp, b'u'),
            KittyKey::Tilde(n) => (n, b'~'),
            KittyKey::Letter(l) => (1, l),
        };
        let mut w = SeqWriter::new(buf);
        w.csi()
            .dec(number)
            .byte(b';')
            .dec(u32::from(mod_param))
            .byte(b':')
            .dec(u32::from(event_type))
            .byte(terminator);
        Some(w.len())
    }

    // ── Mouse encoding ────────────────────────────────────────────────────────

    fn encode_mouse_x10(&self, ev: MouseEvent, buf: &mut [u8]) -> usize {
        // X10: `CSI M <btn+32> <col+32> <row+32>`
        // Normal/ButtonMotion/AnyEvent all use same encoding, differing only
        // in release: button code = 3.
        let mut btn = ev.button.x10_code();
        if ev.mods.shift() {
            btn |= 0x04;
        }
        if ev.mods.alt() {
            btn |= 0x08;
        }
        if ev.mods.ctrl() {
            btn |= 0x10;
        }
        if matches!(ev.state, ButtonState::Released) && !matches!(self.mouse_mode, MouseMode::X10) {
            btn = 3;
        }
        // Motion bit (bit 5 / 0x20) marks a `ButtonMotion`/`AnyEvent` drag or
        // hover report rather than a fresh press/release.
        if matches!(ev.state, ButtonState::Moved) {
            btn |= 0x20;
        }
        // Clamp to 223 (max encodable in 1 byte + 32).
        let col = (ev.col + 1).min(223) as u8;
        let row = (ev.row + 1).min(223) as u8;
        let mut w = SeqWriter::new(buf);
        w.csi()
            .byte(b'M')
            .byte(btn.saturating_add(32))
            .byte(col.saturating_add(32))
            .byte(row.saturating_add(32));
        w.len()
    }

    /// SGR mouse encoding (`CSI < btn ; col ; row M/m`) — independent of
    /// encoder state, so it takes no `&self`.
    fn encode_mouse_sgr(ev: MouseEvent, buf: &mut [u8]) -> usize {
        let mut btn = u16::from(ev.button.x10_code());
        if ev.mods.shift() {
            btn |= 0x04;
        }
        if ev.mods.alt() {
            btn |= 0x08;
        }
        if ev.mods.ctrl() {
            btn |= 0x10;
        }
        if matches!(ev.state, ButtonState::Moved) {
            btn |= 0x20;
        }
        let final_byte: u8 = match ev.state {
            // SGR has no separate "motion" terminator -- a moved report
            // still ends in 'M' (only a genuine release uses 'm'); the
            // motion bit above is what tells the app it wasn't a fresh
            // press.
            ButtonState::Pressed | ButtonState::Moved => b'M',
            ButtonState::Released => b'm',
        };
        let mut w = SeqWriter::new(buf);
        w.csi()
            .byte(b'<')
            .dec(u32::from(btn))
            .byte(b';')
            .dec(u32::from(ev.col + 1))
            .byte(b';')
            .dec(u32::from(ev.row + 1))
            .byte(final_byte);
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
        self.csi()
            .bytes(ps)
            .byte(b';')
            .dec(u32::from(mod_param))
            .byte(final_byte)
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

/// Encode a character respecting shift and caps-lock, writing UTF-8 bytes
/// into `tmp` (>= 4 bytes) and returning the written slice.
///
/// ASCII letters and the standard US-QWERTY punctuation row have an
/// explicit shifted form (1 -> !, a -> A, ...). Anything else -- accented
/// letters, CJK, emoji, or any other Unicode scalar the platform layer
/// hands us via Key::Char -- is passed through as its own UTF-8 encoding
/// rather than collapsed to a placeholder space, so IME/compose input and
/// clipboard paste-as-keys round-trip correctly.
trait EncodeTerminalChar {
    fn encode_terminal(self, shift: bool, caps: bool, tmp: &mut [u8; 4]) -> &[u8];

    /// Ctrl+letter → C0 byte (a=1 … z=26, space=0, @=0, [=27, \=28, ]=29, ^=30, _=31).
    ///
    /// Was a free `ctrl_byte(c: char)` function -- its sole argument is the
    /// type this trait already extends, so it belongs beside
    /// `encode_terminal` as another `char` classification.
    fn ctrl_byte(self) -> Option<u8>;

    /// The shifted form of a US-QWERTY key. Returns `None` for characters
    /// with no conventional shifted form (caller falls through to the
    /// char's own encoding).
    ///
    /// Was a free `shifted_ascii(c: char)` function -- same reasoning as
    /// `ctrl_byte` above.
    fn shifted_ascii(self) -> Option<u8>;
}

impl EncodeTerminalChar for char {
    fn encode_terminal(self, shift: bool, caps: bool, tmp: &mut [u8; 4]) -> &[u8] {
        if shift || caps {
            if let Some(s) = self.shifted_ascii() {
                tmp[0] = s;
                return &tmp[..1];
            }
        } else if self.is_ascii() {
            tmp[0] = self as u8;
            return &tmp[..1];
        }
        let s = self.encode_utf8(tmp);
        let len = s.len();
        &tmp[..len]
    }

    fn ctrl_byte(self) -> Option<u8> {
        match self {
            'a'..='z' => Some(self as u8 - b'a' + 1),
            'A'..='Z' => Some(self as u8 - b'A' + 1),
            // `@`/space and the digit aliases collapse to the same C0 codes
            // (Ctrl+2 = NUL, Ctrl+3 = ESC, … Ctrl+7 = US, Ctrl+8 = DEL).
            '@' | ' ' | '2' => Some(0),
            '[' | '3' => Some(27),
            '\\' | '4' => Some(28),
            ']' | '5' => Some(29),
            '^' | '6' => Some(30),
            '_' | '7' => Some(31),
            '8' => Some(127),
            _ => None,
        }
    }

    fn shifted_ascii(self) -> Option<u8> {
        Some(match self {
            'a'..='z' => self as u8 - b'a' + b'A',
            '1' => b'!',
            '2' => b'@',
            '3' => b'#',
            '4' => b'$',
            '5' => b'%',
            '6' => b'^',
            '7' => b'&',
            '8' => b'*',
            '9' => b'(',
            '0' => b')',
            '-' => b'_',
            '=' => b'+',
            '[' => b'{',
            ']' => b'}',
            '\\' => b'|',
            ';' => b':',
            '\'' => b'"',
            '`' => b'~',
            ',' => b'<',
            '.' => b'>',
            '/' => b'?',
            _ => return None,
        })
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

impl KittyKey {
    /// Classify `key` for kitty encoding.  Returns `None` for keys with no
    /// mapping.
    ///
    /// Was a free `kitty_key(key)` function -- moved onto `KittyKey` as the
    /// `Key -> KittyKey` conversion it actually is, mirroring the
    /// `T::from_code`/`T::from_raw` convention `platform::protocol` uses for
    /// its wire-code conversions (`PointerButton::from_code`,
    /// `ButtonState::from_raw`, …).
    const fn from_key(key: Key) -> Option<Self> {
        use KittyKey::{Letter, Tilde, Unicode};
        Some(match key {
            Key::Char(c) => Unicode(c as u32),
            Key::Enter => Unicode(13),
            Key::Backspace => Unicode(127),
            Key::Tab => Unicode(9),
            Key::Esc => Unicode(27),
            // Legacy tilde-form functional keys.
            Key::Insert => Tilde(2),
            Key::Delete => Tilde(3),
            Key::PageUp => Tilde(5),
            Key::PageDown => Tilde(6),
            // Legacy letter-form functional keys.
            Key::Up => Letter(b'A'),
            Key::Down => Letter(b'B'),
            Key::Right => Letter(b'C'),
            Key::Left => Letter(b'D'),
            Key::Home => Letter(b'H'),
            Key::End => Letter(b'F'),
            // F1–F4 use letter forms; F5+ use tilde forms with their legacy numbers.
            Key::F(1) => Letter(b'P'),
            Key::F(2) => Letter(b'Q'),
            Key::F(3) => Tilde(13),
            Key::F(4) => Letter(b'S'),
            Key::F(5) => Tilde(15),
            Key::F(6) => Tilde(17),
            Key::F(7) => Tilde(18),
            Key::F(8) => Tilde(19),
            Key::F(9) => Tilde(20),
            Key::F(10) => Tilde(21),
            Key::F(11) => Tilde(23),
            Key::F(12) => Tilde(24),
            // Keys with no legacy form — canonical PUA codepoints (kitty spec).
            Key::KpEnter => Unicode(57414),
            Key::CapsLock => Unicode(57358),
            Key::ScrollLock => Unicode(57359),
            Key::NumLock => Unicode(57360),
            Key::Menu => Unicode(57363),
            Key::Shift => Unicode(57441),
            Key::Ctrl => Unicode(57442),
            Key::Alt => Unicode(57443),
            Key::Super => Unicode(57444),
            _ => return None,
        })
    }
}

impl SeqWriter<'_> {
    /// Encode a function key sequence, with modifier support.
    ///
    /// Was a free `encode_fkey(buf, n, mods)` function -- moved onto
    /// `SeqWriter` alongside its sibling `csi_mod`/`fkey_tilde` sequence
    /// builders, since it just composes them.
    fn fkey(&mut self, n: u8, mods: Modifiers) -> Option<&mut Self> {
        // F1–F4 without mods → SS3 P/Q/R/S; with mods → CSI 1 ; <mod> P/Q/R/S
        // F5–F12 → CSI tilde with numeric codes; with mods → CSI num ; mod ~
        match n {
            1..=4 => {
                let letter = b'P' + n - 1;
                if mods.any_active() {
                    self.csi_mod(b"1", mods, letter);
                } else {
                    self.esc().byte(b'O').byte(letter);
                }
            }
            5 => {
                self.fkey_tilde(b"15", mods);
            }
            6 => {
                self.fkey_tilde(b"17", mods);
            }
            7 => {
                self.fkey_tilde(b"18", mods);
            }
            8 => {
                self.fkey_tilde(b"19", mods);
            }
            9 => {
                self.fkey_tilde(b"20", mods);
            }
            10 => {
                self.fkey_tilde(b"21", mods);
            }
            11 => {
                self.fkey_tilde(b"23", mods);
            }
            12 => {
                self.fkey_tilde(b"24", mods);
            }
            _ => return None,
        }
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::{Key, KeyState, Modifiers};
    use crate::mouse::{ButtonState, MouseButton, MouseEvent};

    fn enc() -> Encoder {
        Encoder::new()
    }
    fn buf() -> [u8; 64] {
        [0u8; 64]
    }

    fn encode(enc: &Encoder, key: Key, mods: Modifiers) -> Vec<u8> {
        let mut b = buf();
        let n = enc
            .encode_key(key, KeyState::Pressed, mods, &mut b)
            .unwrap_or(0);
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
        assert_eq!(encode(&e, Key::Up, Modifiers::NONE), b"\x1b[A");
        assert_eq!(encode(&e, Key::Down, Modifiers::NONE), b"\x1b[B");
        assert_eq!(encode(&e, Key::Right, Modifiers::NONE), b"\x1b[C");
        assert_eq!(encode(&e, Key::Left, Modifiers::NONE), b"\x1b[D");
    }

    #[test]
    fn arrows_app_cursor() {
        let mut e = enc();
        e.app_cursor = true;
        assert_eq!(encode(&e, Key::Up, Modifiers::NONE), b"\x1bOA");
        assert_eq!(encode(&e, Key::Down, Modifiers::NONE), b"\x1bOB");
        assert_eq!(encode(&e, Key::Right, Modifiers::NONE), b"\x1bOC");
        assert_eq!(encode(&e, Key::Left, Modifiers::NONE), b"\x1bOD");
    }

    #[test]
    fn arrows_modified() {
        let e = enc();
        assert_eq!(encode(&e, Key::Up, Modifiers::SHIFT), b"\x1b[1;2A");
        assert_eq!(encode(&e, Key::Up, Modifiers::CTRL), b"\x1b[1;5A");
        assert_eq!(
            encode(&e, Key::Up, Modifiers::SHIFT | Modifiers::CTRL),
            b"\x1b[1;6A"
        );
    }

    #[test]
    fn fkeys_no_mods() {
        let e = enc();
        assert_eq!(encode(&e, Key::F(1), Modifiers::NONE), b"\x1bOP");
        assert_eq!(encode(&e, Key::F(2), Modifiers::NONE), b"\x1bOQ");
        assert_eq!(encode(&e, Key::F(5), Modifiers::NONE), b"\x1b[15~");
        assert_eq!(encode(&e, Key::F(12), Modifiers::NONE), b"\x1b[24~");
    }

    #[test]
    fn fkeys_with_mods() {
        let e = enc();
        assert_eq!(encode(&e, Key::F(1), Modifiers::SHIFT), b"\x1b[1;2P");
        assert_eq!(encode(&e, Key::F(5), Modifiers::CTRL), b"\x1b[15;5~");
    }

    #[test]
    fn control_chars() {
        let e = enc();
        assert_eq!(encode(&e, Key::Enter, Modifiers::NONE), b"\r");
        assert_eq!(encode(&e, Key::Backspace, Modifiers::NONE), b"\x7f");
        assert_eq!(encode(&e, Key::Tab, Modifiers::NONE), b"\t");
        assert_eq!(encode(&e, Key::Tab, Modifiers::SHIFT), b"\x1b[Z");
        assert_eq!(encode(&e, Key::Esc, Modifiers::NONE), b"\x1b");
        assert_eq!(encode(&e, Key::Delete, Modifiers::NONE), b"\x1b[3~");
    }

    #[test]
    fn mouse_sgr_press() {
        let mut e = enc();
        e.mouse_mode = MouseMode::Sgr;
        let ev = MouseEvent {
            button: MouseButton::Left,
            state: ButtonState::Pressed,
            col: 4,
            row: 2,
            mods: Modifiers::NONE,
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
            state: ButtonState::Released,
            col: 0,
            row: 0,
            mods: Modifiers::NONE,
        };
        let mut b = buf();
        let n = e.encode_mouse(ev, &mut b).unwrap();
        assert_eq!(&b[..n], b"\x1b[<0;1;1m");
    }

    #[test]
    fn kitty_mode_plain() {
        // With "report all keys" (0b1000) even a bare letter gets full CSI-u
        // form -- this is the mode games/text-UIs opt into explicitly; it is
        // NOT what a freshly pushed Kitty mode with no flags does (see
        // `plain_text_keys_stay_legacy_bytes_with_zero_kitty_flags`).
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1000;
        let mut b = buf();
        let n = e
            .encode_key(Key::Char('a'), KeyState::Pressed, Modifiers::NONE, &mut b)
            .unwrap();
        // CSI 97 ; 1 : 1 u
        assert_eq!(&b[..n], b"\x1b[97;1:1u");
    }

    #[test]
    fn kitty_functional_keys_use_correct_terminators() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1;
        // Letter-form functionals: CSI 1 ; <mod> : <evt> <letter>
        assert_eq!(encode(&e, Key::Up, Modifiers::NONE), b"\x1b[1;1:1A");
        assert_eq!(encode(&e, Key::Home, Modifiers::NONE), b"\x1b[1;1:1H");
        // Tilde-form functionals keep their legacy numbers (no PUA aliasing).
        assert_eq!(encode(&e, Key::Insert, Modifiers::NONE), b"\x1b[2;1:1~");
        assert_eq!(encode(&e, Key::Delete, Modifiers::NONE), b"\x1b[3;1:1~");
        assert_eq!(encode(&e, Key::PageUp, Modifiers::NONE), b"\x1b[5;1:1~");
        assert_eq!(encode(&e, Key::PageDown, Modifiers::NONE), b"\x1b[6;1:1~");
        // PUA-only keys keep the `u` terminator with their canonical codepoints.
        assert_eq!(
            encode(&e, Key::CapsLock, Modifiers::NONE),
            b"\x1b[57358;1:1u"
        );
        assert_eq!(
            encode(&e, Key::ScrollLock, Modifiers::NONE),
            b"\x1b[57359;1:1u"
        );
        assert_eq!(
            encode(&e, Key::NumLock, Modifiers::NONE),
            b"\x1b[57360;1:1u"
        );
    }

    #[test]
    fn kitty_mode_release() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        let mut b = buf();
        // Without the "report event types" flag (0b10) a release must NOT be
        // reported -- apps that didn't ask for it re-insert it as a second
        // press, doubling every typed key.
        assert!(
            e.encode_key(Key::Char('a'), KeyState::Released, Modifiers::NONE, &mut b)
                .is_none()
        );
        // With report-event-types AND report-all-keys, a release of a plain
        // text key reports full CSI-u event type 3 (report-all-keys is
        // needed too -- otherwise a release of a text key still has no
        // CSI-u representation to report release-vs-press *of*).
        e.kitty_flags = 0b10 | 0b1000;
        let n = e
            .encode_key(Key::Char('a'), KeyState::Released, Modifiers::NONE, &mut b)
            .unwrap();
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

    #[test]
    fn number_row_shifted_full() {
        let e = enc();
        let pairs = [
            ('3', '#'),
            ('4', '$'),
            ('5', '%'),
            ('7', '&'),
            ('8', '*'),
            ('9', '('),
            ('0', ')'),
        ];
        for (base, shifted) in pairs {
            let expected = shifted.to_string();
            assert_eq!(
                encode(&e, Key::Char(base), Modifiers::SHIFT),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn shifted_letters_uppercase_the_whole_alphabet() {
        let e = enc();
        for c in 'a'..='z' {
            let expected = c.to_ascii_uppercase().to_string();
            assert_eq!(
                encode(&e, Key::Char(c), Modifiers::SHIFT),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn shift_on_char_with_no_shifted_mapping_falls_back_to_plain() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('!'), Modifiers::SHIFT), b"!");
    }

    // ── Keypad (encode_keypad) ──────────────────────────────────────────

    #[test]
    fn kp_enter_plain_is_cr_app_is_ss3_m() {
        let e = enc();
        assert_eq!(encode(&e, Key::KpEnter, Modifiers::NONE), b"\r");
        let mut app = enc();
        app.app_keypad = true;
        assert_eq!(encode(&app, Key::KpEnter, Modifiers::NONE), b"\x1bOM");
    }

    #[test]
    fn kp_arrows_plain_csi_app_ss3() {
        let e = enc();
        assert_eq!(encode(&e, Key::KpUp, Modifiers::NONE), b"\x1b[A");
        assert_eq!(encode(&e, Key::KpDown, Modifiers::NONE), b"\x1b[B");
        assert_eq!(encode(&e, Key::KpRight, Modifiers::NONE), b"\x1b[C");
        assert_eq!(encode(&e, Key::KpLeft, Modifiers::NONE), b"\x1b[D");

        let mut app = enc();
        app.app_keypad = true;
        assert_eq!(encode(&app, Key::KpUp, Modifiers::NONE), b"\x1bOA");
        assert_eq!(encode(&app, Key::KpDown, Modifiers::NONE), b"\x1bOB");
        assert_eq!(encode(&app, Key::KpRight, Modifiers::NONE), b"\x1bOC");
        assert_eq!(encode(&app, Key::KpLeft, Modifiers::NONE), b"\x1bOD");
    }

    #[test]
    fn kp_arrows_also_go_ss3_under_plain_app_cursor_without_app_keypad() {
        // kp_arrow follows *either* app_keypad or app_cursor -- app_cursor
        // alone (app_keypad false) must still select SS3.
        let mut e = enc();
        e.app_cursor = true;
        assert_eq!(encode(&e, Key::KpUp, Modifiers::NONE), b"\x1bOA");
    }

    #[test]
    fn kp_navigation_cluster_plain_vs_app() {
        let e = enc();
        assert_eq!(encode(&e, Key::KpHome, Modifiers::NONE), b"\x1b[H");
        assert_eq!(encode(&e, Key::KpEnd, Modifiers::NONE), b"\x1b[F");
        assert_eq!(encode(&e, Key::KpPageUp, Modifiers::NONE), b"\x1b[5~");
        assert_eq!(encode(&e, Key::KpPageDown, Modifiers::NONE), b"\x1b[6~");
        assert_eq!(encode(&e, Key::KpBegin, Modifiers::NONE), b"\x1b[E");
        assert_eq!(encode(&e, Key::KpInsert, Modifiers::NONE), b"\x1b[2~");
        assert_eq!(encode(&e, Key::KpDelete, Modifiers::NONE), b"\x1b[3~");

        let mut app = enc();
        app.app_keypad = true;
        assert_eq!(encode(&app, Key::KpHome, Modifiers::NONE), b"\x1bOw");
        assert_eq!(encode(&app, Key::KpEnd, Modifiers::NONE), b"\x1bOq");
        assert_eq!(encode(&app, Key::KpPageUp, Modifiers::NONE), b"\x1bOy");
        assert_eq!(encode(&app, Key::KpPageDown, Modifiers::NONE), b"\x1bOs");
        assert_eq!(encode(&app, Key::KpBegin, Modifiers::NONE), b"\x1bOu");
        assert_eq!(encode(&app, Key::KpInsert, Modifiers::NONE), b"\x1bOp");
        assert_eq!(encode(&app, Key::KpDelete, Modifiers::NONE), b"\x1bOn");
    }

    #[test]
    fn kp_char_digits_plain_pass_through_literally() {
        let e = enc();
        for c in ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'] {
            let mut b = buf();
            let n = e
                .encode_key(Key::KpChar(c), KeyState::Pressed, Modifiers::NONE, &mut b)
                .unwrap();
            assert_eq!(&b[..n], c.to_string().as_bytes(), "digit {c}");
        }
    }

    #[test]
    fn kp_char_operators_plain_pass_through_literally() {
        let e = enc();
        for c in ['.', '+', '-', '*', '/'] {
            let mut b = buf();
            let n = e
                .encode_key(Key::KpChar(c), KeyState::Pressed, Modifiers::NONE, &mut b)
                .unwrap();
            assert_eq!(&b[..n], c.to_string().as_bytes(), "operator {c}");
        }
    }

    #[test]
    fn kp_char_digits_app_keypad_use_ss3_codes() {
        let mut e = enc();
        e.app_keypad = true;
        let pairs = [
            ('0', b'p'),
            ('1', b'q'),
            ('2', b'r'),
            ('3', b's'),
            ('4', b't'),
            ('5', b'u'),
            ('6', b'v'),
            ('7', b'w'),
            ('8', b'x'),
            ('9', b'y'),
        ];
        for (c, code) in pairs {
            let mut b = buf();
            let n = e
                .encode_key(Key::KpChar(c), KeyState::Pressed, Modifiers::NONE, &mut b)
                .unwrap();
            assert_eq!(&b[..n], [b'\x1b', b'O', code], "digit {c}");
        }
    }

    #[test]
    fn kp_char_operators_app_keypad_use_ss3_codes() {
        let mut e = enc();
        e.app_keypad = true;
        let pairs = [
            ('.', b'n'),
            ('+', b'l'),
            ('-', b'm'),
            ('*', b'j'),
            ('/', b'o'),
        ];
        for (c, code) in pairs {
            let mut b = buf();
            let n = e
                .encode_key(Key::KpChar(c), KeyState::Pressed, Modifiers::NONE, &mut b)
                .unwrap();
            assert_eq!(&b[..n], [b'\x1b', b'O', code], "operator {c}");
        }
    }

    #[test]
    fn kp_char_unrecognised_char_in_app_keypad_produces_none() {
        // Any char not in the 0-9/./+/-/*// table falls through to None when
        // app_keypad is on (there is no SS3 code for it).
        let mut e = enc();
        e.app_keypad = true;
        let mut b = buf();
        let r = e.encode_key(Key::KpChar('x'), KeyState::Pressed, Modifiers::NONE, &mut b);
        assert!(r.is_none());
    }

    // ── kitty_key / encode_kitty coverage ────

    fn kitty(e: &Encoder, key: Key, state: KeyState, mods: Modifiers) -> Vec<u8> {
        let mut b = buf();
        let n = e.encode_key(key, state, mods, &mut b).unwrap_or(0);
        b[..n].to_vec()
    }

    #[test]
    fn kitty_unmapped_key_produces_none() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1000; // report all keys, so this is genuinely unmapped
        let mut b = buf();
        let r = e.encode_key(Key::KpUp, KeyState::Pressed, Modifiers::NONE, &mut b);
        assert!(r.is_none());
    }

    #[test]
    fn kitty_navigation_letter_form_keys() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // Functional/navigation keys get CSI-u treatment once "disambiguate
        // escape codes" (0b1) is requested -- exactly what fish always asks
        // for, so this is the realistic baseline for these keys.
        e.kitty_flags = 0b1;
        assert_eq!(
            kitty(&e, Key::Up, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1A"
        );
        assert_eq!(
            kitty(&e, Key::Down, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1B"
        );
        assert_eq!(
            kitty(&e, Key::Right, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1C"
        );
        assert_eq!(
            kitty(&e, Key::Left, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1D"
        );
        assert_eq!(
            kitty(&e, Key::Home, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1H"
        );
        assert_eq!(
            kitty(&e, Key::End, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1F"
        );
    }

    #[test]
    fn kitty_tilde_form_keys() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1;
        assert_eq!(
            kitty(&e, Key::Insert, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[2;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::Delete, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[3;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::PageUp, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[5;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::PageDown, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[6;1:1~"
        );
    }

    #[test]
    fn kitty_function_keys_f1_through_f12() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1;
        assert_eq!(
            kitty(&e, Key::F(1), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1P"
        );
        assert_eq!(
            kitty(&e, Key::F(2), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1Q"
        );
        assert_eq!(
            kitty(&e, Key::F(3), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[13;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(4), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[1;1:1S"
        );
        assert_eq!(
            kitty(&e, Key::F(5), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[15;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(6), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[17;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(7), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[18;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(8), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[19;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(9), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[20;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(10), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[21;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(11), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[23;1:1~"
        );
        assert_eq!(
            kitty(&e, Key::F(12), KeyState::Pressed, Modifiers::NONE),
            b"\x1b[24;1:1~"
        );
    }

    #[test]
    fn kitty_special_pua_only_keys() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // KpEnter has a legacy fallback (`\r`, same as plain Enter) that
        // takes priority unless the app asked to see it as a distinct key
        // via "report all keys" -- exercise that mode to see its PUA form.
        e.kitty_flags = 0b1000;
        assert_eq!(
            kitty(&e, Key::KpEnter, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57414;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::ScrollLock, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57359;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::NumLock, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57360;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Menu, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57363;1:1u"
        );
    }

    #[test]
    fn kitty_bare_modifier_keys_report_their_own_pua_codepoints() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // Modifier-key press events are only reported at all once "report
        // all keys" is requested (spec: "with this mode, events for pressing
        // modifier keys are reported").
        e.kitty_flags = 0b1000;
        assert_eq!(
            kitty(&e, Key::Shift, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57441;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Ctrl, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57442;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Alt, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57443;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Super, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[57444;1:1u"
        );
    }

    #[test]
    fn kitty_control_chars_use_their_c0_codepoint() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // Enter/Backspace/Tab keep their legacy bytes even with "disambiguate"
        // on (the spec's explicit `reset`-after-crash exception) -- only
        // "report all keys" gives them CSI-u form. Esc has no legacy form of
        // its own under Kitty disambiguation, so it needs only that flag.
        e.kitty_flags = 0b1000;
        assert_eq!(
            kitty(&e, Key::Enter, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[13;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Backspace, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[127;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Tab, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[9;1:1u"
        );
        assert_eq!(
            kitty(&e, Key::Esc, KeyState::Pressed, Modifiers::NONE),
            b"\x1b[27;1:1u"
        );
    }

    #[test]
    fn enter_backspace_tab_keep_legacy_bytes_under_disambiguate_alone() {
        // Without "report all keys", Enter/Backspace/Tab must NOT switch to
        // CSI-u even once "disambiguate" is on -- this is what lets `reset`
        // still work at a shell prompt after a crashed program leaves Kitty
        // mode enabled, and is exactly the case `fish` leaves other programs
        // in (fish requests only 0b1, never 0b1000).
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        e.kitty_flags = 0b1;
        assert_eq!(kitty(&e, Key::Enter, KeyState::Pressed, Modifiers::NONE), b"\r");
        assert_eq!(
            kitty(&e, Key::Backspace, KeyState::Pressed, Modifiers::NONE),
            b"\x7f"
        );
        assert_eq!(kitty(&e, Key::Tab, KeyState::Pressed, Modifiers::NONE), b"\t");
    }

    #[test]
    fn plain_text_keys_stay_legacy_bytes_with_zero_kitty_flags() {
        // The actual regression this module exists to prevent: a freshly
        // pushed Kitty mode with no flags set (what `fish` leaves behind for
        // anything it execs, since fish itself only ever requests 0b1) must
        // encode a bare letter as its own UTF-8 byte, not `CSI 97;1:1u`.
        // Getting this wrong meant every keystroke typed into a raw-mode
        // password reader (which never negotiates Kitty mode itself) showed
        // up as several bogus characters instead of the one that was typed.
        let e = {
            let mut e = enc();
            e.keyboard_mode = KeyboardMode::Kitty;
            e
        };
        assert_eq!(kitty(&e, Key::Char('a'), KeyState::Pressed, Modifiers::NONE), b"a");
        assert_eq!(kitty(&e, Key::Char('Z'), KeyState::Pressed, Modifiers::SHIFT), b"Z");
        assert_eq!(kitty(&e, Key::Enter, KeyState::Pressed, Modifiers::NONE), b"\r");
    }

    #[test]
    fn kitty_repeat_state_reports_event_type_2_uniformly() {
        let mut e = enc();
        e.keyboard_mode = KeyboardMode::Kitty;
        // report-all-keys so plain 'a' gets CSI-u form, report-event-types
        // so the repeat is distinguishable (event type 2) from a press.
        e.kitty_flags = 0b1000 | 0b10;
        assert_eq!(
            kitty(&e, Key::Char('a'), KeyState::Repeated, Modifiers::NONE),
            b"\x1b[97;1:2u"
        );
        e.kitty_flags = 0b1 | 0b10;
        assert_eq!(
            kitty(&e, Key::Up, KeyState::Repeated, Modifiers::NONE),
            b"\x1b[1;1:2A"
        );
    }

    // ── ctrl_byte / shifted_ascii aliasing coverage ──

    #[test]
    fn ctrl_digit_aliases_match_symbol_aliases() {
        let e = enc();
        assert_eq!(
            encode(&e, Key::Char('2'), Modifiers::CTRL),
            encode(&e, Key::Char('@'), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('2'), Modifiers::CTRL),
            encode(&e, Key::Char(' '), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('3'), Modifiers::CTRL),
            encode(&e, Key::Char('['), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('4'), Modifiers::CTRL),
            encode(&e, Key::Char('\\'), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('5'), Modifiers::CTRL),
            encode(&e, Key::Char(']'), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('6'), Modifiers::CTRL),
            encode(&e, Key::Char('^'), Modifiers::CTRL)
        );
        assert_eq!(
            encode(&e, Key::Char('7'), Modifiers::CTRL),
            encode(&e, Key::Char('_'), Modifiers::CTRL)
        );
        assert_eq!(encode(&e, Key::Char('8'), Modifiers::CTRL), vec![127]);
    }

    #[test]
    fn ctrl_unmapped_char_falls_through_to_plain_encoding() {
        let e = enc();
        assert_eq!(encode(&e, Key::Char('9'), Modifiers::CTRL), b"9");
    }

    #[test]
    fn shifted_ascii_full_punctuation_row() {
        let e = enc();
        let pairs = [
            ('-', '_'),
            ('=', '+'),
            ('[', '{'),
            (']', '}'),
            ('\\', '|'),
            (';', ':'),
            ('\'', '"'),
            ('`', '~'),
            (',', '<'),
            ('.', '>'),
            ('/', '?'),
        ];
        for (base, shifted) in pairs {
            assert_eq!(
                encode(&e, Key::Char(base), Modifiers::SHIFT),
                shifted.to_string().as_bytes(),
                "{base} shifted"
            );
        }
    }

    #[test]
    fn kp_char_non_ascii_plain_passes_through_utf8() {
        // encode_keypad's plain-mode branch just forwards the char's own
        // UTF-8 bytes, same machinery as encode_char -- verify it isn't
        // secretly ASCII-only.
        let e = enc();
        let c = 'é';
        let mut b = buf();
        let n = e
            .encode_key(Key::KpChar(c), KeyState::Pressed, Modifiers::NONE, &mut b)
            .unwrap();
        assert_eq!(&b[..n], c.to_string().as_bytes());
    }
}
