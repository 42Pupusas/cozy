//! VT/ANSI parser state machine.
//!
//! Implements the Paul Williams DEC ANSI parser model.
//! Reference: <https://vt100.net/emu/dec_ansi_parser>

use crate::event::{CsiAction, OscCmd, SgrAttr, C0};
use crate::params::Params;
use crate::Event;

/// DEC ANSI parser states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(dead_code)] // DCS sub-states are stubs; wired up in M10
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    SosPmApcString,
    // UTF-8 multibyte decoding
    Utf8(u8, u32, u8), // (bytes_remaining, accumulated, total_bytes)
}

/// Maximum OSC string length we buffer.
const OSC_BUF_MAX: usize = 512;

/// The stateful VT/ANSI byte-stream parser.
///
/// Feed bytes one at a time via [`Parser::advance`], or a whole slice via
/// [`Parser::feed`]. Each call returns an optional [`Event`].
pub struct Parser {
    state: State,
    /// CSI/DCS numeric parameters.
    params: Params,
    /// CSI intermediate byte (e.g. `?` for DEC private modes).
    intermediate: Option<u8>,
    /// ESC intermediate byte.
    esc_intermediate: Option<u8>,
    /// OSC string accumulation buffer.
    osc_buf: [u8; OSC_BUF_MAX],
    osc_len: usize,
}

impl Parser {
    /// Create a new parser in the Ground state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::new(),
            intermediate: None,
            esc_intermediate: None,
            osc_buf: [0u8; OSC_BUF_MAX],
            osc_len: 0,
        }
    }

    /// Feed a slice of bytes, invoking `callback` for each event produced.
    ///
    /// This is the primary hot-path entry point. A single byte can produce
    /// at most one event.
    pub fn feed(&mut self, bytes: &[u8], mut callback: impl FnMut(Event)) {
        for &b in bytes {
            if let Some(ev) = self.advance(b) {
                callback(ev);
            }
        }
    }

    /// Advance the state machine by one byte.
    ///
    /// Returns `Some(Event)` when a complete sequence has been recognised,
    /// or `None` when more bytes are needed.
    #[inline]
    pub fn advance(&mut self, byte: u8) -> Option<Event> {
        // ── Anywhere transitions (C1 controls, CAN, SUB, ESC) ────────────
        match byte {
            // CAN (0x18) and SUB (0x1A) abort any in-progress sequence.
            0x18 | 0x1A => {
                self.state = State::Ground;
                return Some(Event::Control(C0::from_byte(byte)));
            }
            // ESC starts a new escape sequence from any state.
            0x1B => {
                self.reset_seq();
                self.state = State::Escape;
                return None;
            }
            // DEL (0x7F) is ignored everywhere.
            0x7F => return None,
            _ => {}
        }

        match self.state {
            // ── Ground ───────────────────────────────────────────────────
            State::Ground => self.ground(byte),

            // ── UTF-8 multibyte ──────────────────────────────────────────
            State::Utf8(remaining, acc, total) => self.utf8_continue(byte, remaining, acc, total),

            // ── ESC ──────────────────────────────────────────────────────
            State::Escape => self.escape(byte),
            State::EscapeIntermediate => self.escape_intermediate(byte),

            // ── CSI ──────────────────────────────────────────────────────
            State::CsiEntry => self.csi_entry(byte),
            State::CsiParam => self.csi_param(byte),
            State::CsiIntermediate => self.csi_intermediate(byte),
            State::CsiIgnore => {
                if byte.is_ascii_alphabetic() || byte == b'~' {
                    self.state = State::Ground;
                }
                None
            }

            // ── OSC ──────────────────────────────────────────────────────
            State::OscString => self.osc_string(byte),

            // ── DCS / SOS / PM / APC ─────────────────────────────────────
            State::DcsEntry
            | State::DcsParam
            | State::DcsIntermediate
            | State::DcsPassthrough
            | State::DcsIgnore
            | State::SosPmApcString => self.dcs_or_sos(byte),
        }
    }

    // ── Ground ────────────────────────────────────────────────────────────

    fn ground(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // C0 controls
            0x00..=0x1F => Some(Event::Control(C0::from_byte(byte))),
            // ASCII printable
            0x20..=0x7E => Some(Event::Print(byte as char)),
            // 2-byte UTF-8 (110xxxxx)
            0xC0..=0xDF => {
                let acc = u32::from(byte & 0x1F);
                self.state = State::Utf8(1, acc, 2);
                None
            }
            // 3-byte UTF-8 (1110xxxx)
            0xE0..=0xEF => {
                let acc = u32::from(byte & 0x0F);
                self.state = State::Utf8(2, acc, 3);
                None
            }
            // 4-byte UTF-8 (11110xxx)
            0xF0..=0xF7 => {
                let acc = u32::from(byte & 0x07);
                self.state = State::Utf8(3, acc, 4);
                None
            }
            // Bare continuation byte or overlong — replace with U+FFFD
            _ => Some(Event::Print('\u{FFFD}')),
        }
    }

    // ── UTF-8 continuation ────────────────────────────────────────────────

    fn utf8_continue(&mut self, byte: u8, remaining: u8, acc: u32, total: u8) -> Option<Event> {
        if byte & 0xC0 != 0x80 {
            // Not a continuation byte — replacement character, restart.
            self.state = State::Ground;
            return Some(Event::Print('\u{FFFD}'));
        }
        let acc = (acc << 6) | u32::from(byte & 0x3F);
        if remaining == 1 {
            self.state = State::Ground;
            let ch = char::from_u32(acc).unwrap_or('\u{FFFD}');
            // Reject overlong encodings.
            let ok = match total {
                2 => acc >= 0x80,
                3 => acc >= 0x800,
                4 => (0x10000..=0x10FFFF).contains(&acc),
                _ => false,
            };
            Some(Event::Print(if ok { ch } else { '\u{FFFD}' }))
        } else {
            self.state = State::Utf8(remaining - 1, acc, total);
            None
        }
    }

    // ── ESC ───────────────────────────────────────────────────────────────

    fn escape(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // Intermediates (space through /)
            0x20..=0x2F => {
                self.esc_intermediate = Some(byte);
                self.state = State::EscapeIntermediate;
                None
            }
            // CSI introducer
            b'[' => {
                self.state = State::CsiEntry;
                None
            }
            // OSC introducer
            b']' => {
                self.osc_len = 0;
                self.state = State::OscString;
                None
            }
            // DCS introducer
            b'P' => {
                self.state = State::DcsEntry;
                None
            }
            // SOS / PM / APC
            b'X' | b'^' | b'_' => {
                self.state = State::SosPmApcString;
                None
            }
            // Final bytes (4/0–7/E)
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(Event::Esc {
                    intermediate: self.esc_intermediate,
                    final_byte: byte,
                })
            }
            _ => {
                self.state = State::Ground;
                None
            }
        }
    }

    fn escape_intermediate(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(Event::Esc {
                    intermediate: self.esc_intermediate,
                    final_byte: byte,
                })
            }
            0x20..=0x2F => {
                // Additional intermediate — overwrite (rare).
                self.esc_intermediate = Some(byte);
                None
            }
            _ => {
                self.state = State::Ground;
                None
            }
        }
    }

    // ── CSI ───────────────────────────────────────────────────────────────

    fn csi_entry(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // Intermediates
            0x20..=0x2F => {
                self.intermediate = Some(byte);
                self.state = State::CsiIntermediate;
                None
            }
            // Parameter digits
            b'0'..=b'9' => {
                self.params.push_digit(byte);
                self.state = State::CsiParam;
                None
            }
            // Semicolon (parameter separator)
            b';' => {
                self.params.next_param();
                self.state = State::CsiParam;
                None
            }
            // DEC private marker or similar (? ! > =)
            0x3C..=0x3F => {
                self.intermediate = Some(byte);
                self.state = State::CsiParam;
                None
            }
            // Final byte
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(self.dispatch_csi(byte))
            }
            _ => {
                self.state = State::CsiIgnore;
                None
            }
        }
    }

    fn csi_param(&mut self, byte: u8) -> Option<Event> {
        match byte {
            b'0'..=b'9' => {
                self.params.push_digit(byte);
                None
            }
            b';' => {
                self.params.next_param();
                None
            }
            // Another intermediate
            0x20..=0x2F => {
                self.state = State::CsiIntermediate;
                None
            }
            // Final byte
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(self.dispatch_csi(byte))
            }
            // Anything else → ignore rest of sequence
            _ => {
                self.state = State::CsiIgnore;
                None
            }
        }
    }

    fn csi_intermediate(&mut self, byte: u8) -> Option<Event> {
        match byte {
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(self.dispatch_csi(byte))
            }
            0x20..=0x2F => None,
            _ => {
                self.state = State::CsiIgnore;
                None
            }
        }
    }

    fn dispatch_csi(&mut self, final_byte: u8) -> Event {
        let p = &self.params;
        let inter = self.intermediate;

        let action = match (inter, final_byte) {
            // CUU
            (None, b'A') => CsiAction::CursorUp(p.get_or(0, 1)),
            // CUD
            (None, b'B') => CsiAction::CursorDown(p.get_or(0, 1)),
            // CUF
            (None, b'C') => CsiAction::CursorForward(p.get_or(0, 1)),
            // CUB
            (None, b'D') => CsiAction::CursorBack(p.get_or(0, 1)),
            // CNL
            (None, b'E') => CsiAction::CursorNextLine(p.get_or(0, 1)),
            // CPL
            (None, b'F') => CsiAction::CursorPrevLine(p.get_or(0, 1)),
            // CHA
            (None, b'G') => CsiAction::CursorHorizAbs(p.get_or(0, 1)),
            // CUP
            (None, b'H') => CsiAction::CursorPosition {
                row: p.get_or(0, 1),
                col: p.get_or(1, 1),
            },
            // ED
            (None, b'J') => CsiAction::EraseDisplay(p.get_raw(0) as u8),
            // EL
            (None, b'K') => CsiAction::EraseLine(p.get_raw(0) as u8),
            // IL
            (None, b'L') => CsiAction::InsertLines(p.get_or(0, 1)),
            // DL
            (None, b'M') => CsiAction::DeleteLines(p.get_or(0, 1)),
            // DCH
            (None, b'P') => CsiAction::DeleteChars(p.get_or(0, 1)),
            // SU
            (None, b'S') => CsiAction::ScrollUp(p.get_or(0, 1)),
            // SD
            (None, b'T') => CsiAction::ScrollDown(p.get_or(0, 1)),
            // ECH
            (None, b'X') => CsiAction::EraseChars(p.get_or(0, 1)),
            // ICH
            (None, b'@') => CsiAction::InsertChars(p.get_or(0, 1)),
            // HVP
            (None, b'f') => CsiAction::HorizVertPos {
                row: p.get_or(0, 1),
                col: p.get_or(1, 1),
            },
            // SGR
            (None, b'm') => CsiAction::Sgr(self.decode_sgr()),
            // DSR
            (None, b'n') => CsiAction::DeviceStatusReport(p.get_raw(0) as u8),
            // DA
            (None, b'c') => CsiAction::DeviceAttributes(p.get_raw(0) as u8),
            // DECSTBM
            (None, b'r') => CsiAction::SetScrollRegion {
                top: p.get_or(0, 1),
                bottom: p.get_or(1, u16::MAX),
            },
            // SM
            (None, b'h') => CsiAction::SetMode(p.get_raw(0)),
            // RM
            (None, b'l') => CsiAction::ResetMode(p.get_raw(0)),
            // DECSET
            (Some(b'?'), b'h') => CsiAction::DecSet(p.get_raw(0)),
            // DECRST
            (Some(b'?'), b'l') => CsiAction::DecRst(p.get_raw(0)),
            // Unknown
            _ => CsiAction::Unknown {
                final_byte,
                params: *p,
            },
        };

        self.reset_seq();
        Event::Csi(action)
    }

    fn decode_sgr(&self) -> heapless::Vec<SgrAttr, 16> {
        let mut attrs: heapless::Vec<SgrAttr, 16> = heapless::Vec::new();
        let p = &self.params;

        if p.is_empty() {
            let _ = attrs.push(SgrAttr::Reset);
            return attrs;
        }

        let mut i = 0usize;
        while i < p.len() {
            let val = p.get_raw(i);
            let attr = match val {
                0 => SgrAttr::Reset,
                1 => SgrAttr::Bold,
                2 => SgrAttr::Dim,
                3 => SgrAttr::Italic,
                4 => SgrAttr::Underline,
                5 => SgrAttr::Blink,
                6 => SgrAttr::RapidBlink,
                7 => SgrAttr::Inverse,
                8 => SgrAttr::Invisible,
                9 => SgrAttr::Strikethrough,
                22 => SgrAttr::NormalIntensity,
                23 => SgrAttr::NoItalic,
                24 => SgrAttr::NoUnderline,
                25 => SgrAttr::NoBlink,
                27 => SgrAttr::NoInverse,
                28 => SgrAttr::NoInvisible,
                29 => SgrAttr::NoStrikethrough,
                30..=37 => SgrAttr::FgStandard((val - 30) as u8),
                38 => {
                    match p.get_raw(i + 1) {
                        5 => {
                            let idx = p.get_raw(i + 2) as u8;
                            i += 2;
                            SgrAttr::Fg256(idx)
                        }
                        2 => {
                            let r = p.get_raw(i + 2) as u8;
                            let g = p.get_raw(i + 3) as u8;
                            let b = p.get_raw(i + 4) as u8;
                            i += 4;
                            SgrAttr::FgRgb(r, g, b)
                        }
                        _ => SgrAttr::FgDefault,
                    }
                }
                39 => SgrAttr::FgDefault,
                40..=47 => SgrAttr::BgStandard((val - 40) as u8),
                48 => {
                    match p.get_raw(i + 1) {
                        5 => {
                            let idx = p.get_raw(i + 2) as u8;
                            i += 2;
                            SgrAttr::Bg256(idx)
                        }
                        2 => {
                            let r = p.get_raw(i + 2) as u8;
                            let g = p.get_raw(i + 3) as u8;
                            let b = p.get_raw(i + 4) as u8;
                            i += 4;
                            SgrAttr::BgRgb(r, g, b)
                        }
                        _ => SgrAttr::BgDefault,
                    }
                }
                49 => SgrAttr::BgDefault,
                90..=97 => SgrAttr::FgStandard((val - 90 + 8) as u8),
                100..=107 => SgrAttr::BgStandard((val - 100 + 8) as u8),
                _ => {
                    i += 1;
                    continue;
                }
            };
            let _ = attrs.push(attr);
            i += 1;
        }
        attrs
    }

    // ── OSC ───────────────────────────────────────────────────────────────

    fn osc_string(&mut self, byte: u8) -> Option<Event> {
        match byte {
            // BEL terminates OSC (xterm extension).
            0x07 => {
                let ev = self.dispatch_osc();
                self.state = State::Ground;
                Some(ev)
            }
            // ST (ESC \) terminates OSC — ESC was already consumed by
            // the anywhere rule; we see the `\` here.
            0x9C | b'\\' => {
                let ev = self.dispatch_osc();
                self.state = State::Ground;
                Some(ev)
            }
            // Accumulate OSC data.
            _ => {
                if self.osc_len < OSC_BUF_MAX {
                    self.osc_buf[self.osc_len] = byte;
                    self.osc_len += 1;
                }
                None
            }
        }
    }

    fn dispatch_osc(&self) -> Event {
        let data = &self.osc_buf[..self.osc_len];

        // Find the ';' separator between Ps and Pt.
        let sep = data.iter().position(|&b| b == b';');
        let (ps_bytes, pt_bytes) = if let Some(pos) = sep {
            (&data[..pos], &data[pos + 1..])
        } else {
            (data, &b""[..])
        };

        let ps: u8 = ps_bytes
            .iter()
            .fold(0u8, |acc, &b| acc.saturating_mul(10).saturating_add(b - b'0'));

        let pt = core::str::from_utf8(pt_bytes).unwrap_or("");

        let cmd = match ps {
            0 | 2 => {
                let mut title = heapless::String::<256>::new();
                for ch in pt.chars().take(256) {
                    let _ = title.push(ch);
                }
                OscCmd::SetTitle(title)
            }
            8 => {
                // OSC 8 ; params ; uri BEL — hyperlink.
                let mut uri = heapless::String::<256>::new();
                // pt may be "params;uri" — skip to second ';'.
                let uri_str = pt.splitn(2, ';').nth(1).unwrap_or(pt);
                for ch in uri_str.chars().take(256) {
                    let _ = uri.push(ch);
                }
                OscCmd::Hyperlink { uri }
            }
            _ => {
                let mut data_s = heapless::String::<256>::new();
                for ch in pt.chars().take(256) {
                    let _ = data_s.push(ch);
                }
                OscCmd::Unknown { ps, data: data_s }
            }
        };

        Event::Osc(cmd)
    }

    // ── DCS / SOS / PM / APC ─────────────────────────────────────────────

    fn dcs_or_sos(&mut self, byte: u8) -> Option<Event> {
        // ST (0x9C) or ESC \ terminates these sequences.
        if byte == 0x9C || byte == b'\\' {
            self.state = State::Ground;
            return Some(Event::Dcs);
        }
        // Otherwise consume silently.
        None
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn reset_seq(&mut self) {
        self.params.clear();
        self.intermediate = None;
        self.esc_intermediate = None;
        self.osc_len = 0;
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut events = Vec::new();
        p.feed(bytes, |ev| events.push(ev));
        events
    }

    #[test]
    fn ascii_text() {
        let events = parse_all(b"hello");
        assert_eq!(
            events,
            vec![
                Event::Print('h'),
                Event::Print('e'),
                Event::Print('l'),
                Event::Print('l'),
                Event::Print('o'),
            ]
        );
    }

    #[test]
    fn cursor_up() {
        let events = parse_all(b"\x1b[3A");
        assert_eq!(events, vec![Event::Csi(CsiAction::CursorUp(3))]);
    }

    #[test]
    fn cursor_position_default() {
        let events = parse_all(b"\x1b[H");
        assert_eq!(
            events,
            vec![Event::Csi(CsiAction::CursorPosition { row: 1, col: 1 })]
        );
    }

    #[test]
    fn sgr_bold_fg_red() {
        let events = parse_all(b"\x1b[1;31m");
        if let Some(Event::Csi(CsiAction::Sgr(attrs))) = events.first() {
            assert!(attrs.contains(&SgrAttr::Bold));
            assert!(attrs.contains(&SgrAttr::FgStandard(1)));
        } else {
            panic!("expected SGR event, got {events:?}");
        }
    }

    #[test]
    fn sgr_reset_empty() {
        let events = parse_all(b"\x1b[m");
        if let Some(Event::Csi(CsiAction::Sgr(attrs))) = events.first() {
            assert!(attrs.contains(&SgrAttr::Reset));
        } else {
            panic!("expected SGR reset, got {events:?}");
        }
    }

    #[test]
    fn osc_title() {
        let events = parse_all(b"\x1b]2;My Terminal\x07");
        assert!(matches!(
            events.first(),
            Some(Event::Osc(OscCmd::SetTitle(_)))
        ));
    }

    #[test]
    fn erase_display() {
        let events = parse_all(b"\x1b[2J");
        assert_eq!(events, vec![Event::Csi(CsiAction::EraseDisplay(2))]);
    }

    #[test]
    fn dec_set() {
        let events = parse_all(b"\x1b[?25h");
        assert_eq!(events, vec![Event::Csi(CsiAction::DecSet(25))]);
    }

    #[test]
    fn utf8_two_byte() {
        let events = parse_all("é".as_bytes());
        assert_eq!(events, vec![Event::Print('é')]);
    }

    #[test]
    fn utf8_emoji() {
        let events = parse_all("🦀".as_bytes());
        assert_eq!(events, vec![Event::Print('🦀')]);
    }

    #[test]
    fn c0_bell() {
        let events = parse_all(b"\x07");
        assert_eq!(events, vec![Event::Control(C0::Bell)]);
    }
}
