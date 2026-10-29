//! VT/ANSI parser state machine.
//!
//! Implements the Paul Williams DEC ANSI parser model.
//! Reference: <https://vt100.net/emu/dec_ansi_parser>

use crate::event::{C0, CsiAction, DcsCmd, OscCmd, SgrAttr, SgrAttrs};
use crate::params::Params;
use crate::scan::Scanner;
use crate::{EventCallback, TermEvent, TerminalSink};

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

/// Maximum DCS payload length we buffer (DECRQSS requests and other DCS
/// payloads are short; Sixel/terminfo-query bodies that exceed this are
/// truncated rather than growing the heap -- same tradeoff as `OSC_BUF_MAX`).
const DCS_BUF_MAX: usize = 256;

/// The stateful VT/ANSI byte-stream parser.
///
/// Feed bytes one at a time via [`Parser::advance`], or a whole slice via
/// [`Parser::feed`]. Each call returns an optional [`TermEvent`].
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
    /// DCS payload accumulation buffer (used for `DcsPassthrough` and
    /// `SosPmApcString` alike -- both just collect bytes until ST).
    dcs_buf: [u8; DCS_BUF_MAX],
    dcs_len: usize,
    /// Set when entering a DCS (vs. SOS/PM/APC) sequence, so the terminator
    /// knows whether to look for a DECRQSS prefix (`$q`) in the buffered
    /// payload -- SOS/PM/APC have no such convention and always dispatch as
    /// `DcsCmd::Unknown`.
    in_dcs: bool,
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
            dcs_buf: [0u8; DCS_BUF_MAX],
            dcs_len: 0,
            in_dcs: false,
        }
    }

    /// Feed a slice of bytes, invoking `callback` for each event produced.
    ///
    /// This is the primary hot-path entry point. A single byte can produce
    /// at most one event. Events borrow from the parser's internal buffers
    /// (see [`TermEvent`]), so each must be fully handled inside `callback`
    /// before the next byte is processed.
    pub fn feed(&mut self, bytes: &[u8], callback: impl for<'a> FnMut(TermEvent<'a>)) {
        self.feed_to(bytes, &mut EventCallback::new(callback));
    }

    /// Feed bytes directly to a [`TerminalSink`].
    ///
    /// This is the production parser-to-grid path. It preserves exactly the
    /// same event ordering as [`Self::feed`] without materialising a
    /// [`TermEvent`] and matching it again for each recognised action.
    pub fn feed_to(&mut self, bytes: &[u8], sink: &mut impl TerminalSink) {
        let mut i = 0;
        let n = bytes.len();
        // One CPU-feature probe per chunk (cached atomic load), reused by
        // every printable-run scan below.
        let scanner = Scanner::new();
        while i < n {
            // ── Plain ASCII run fast path ────────────────────────────────
            // Floods spend most of their bytes in Ground as printable ASCII.
            // Emit one borrowed event for a long run so consumers can process
            // the stream without one callback per byte. Short runs retain the
            // ordinary Print event shape for small event-oriented callers.
            // NOTE: `is_ascii_graphic()` is the WRONG predicate here -- it
            // excludes space (0x20), and real text has a space every ~5
            // characters, which chopped every line into sub-8-byte word
            // fragments that all fell back to the per-char Print path (the
            // run fast path almost never fired on prose). Space is printable,
            // single-width, and charset-safe, so the run covers 0x20..=0x7E
            // (DEL 0x7F stays excluded -- it is a control byte).
            if self.state == State::Ground && matches!(bytes[i], 0x20..=0x7E) {
                let start = i;
                // SIMD scan (SSE2/AVX2 on x86_64, scalar elsewhere): finds
                // the run terminator 16/32 bytes at a time instead of one.
                i += scanner.printable_run_len(&bytes[i..]);
                if i - start >= 8 {
                    sink.print_run(&bytes[start..i]);
                } else {
                    for &byte in &bytes[start..i] {
                        sink.print(char::from(byte));
                    }
                }
                continue;
            }
            if self.state == State::Ground && bytes[i] >= 0x80 {
                let consumed = Self::print_utf8_run(&bytes[i..], sink);
                if consumed > 0 {
                    i += consumed;
                    continue;
                }
            }
            // ── CSI parameter-body fast path ─────────────────────────────
            // In `CsiParam` the body is a run of ASCII digits and `;`
            // separators. Neither trips an "anywhere" transition (0x30..=0x39
            // and 0x3B are none of CAN/SUB/ESC/DEL) and neither changes
            // state — a digit accumulates into the current parameter, a `;`
            // opens the next one. So consuming the whole body here is exactly
            // equivalent to feeding each byte through `advance`, but skips the
            // per-byte anywhere-match + state dispatch that otherwise
            // dominates SGR-heavy streams (truecolor / 256-color are ~half
            // separators). Digit runs fold in one `push_digits` so the
            // accumulator stays in a register across the run.
            if self.state == State::CsiParam {
                let b = bytes[i];
                if b.is_ascii_digit() {
                    let start = i;
                    while i < n && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    self.params.push_digits(&bytes[start..i]);
                    continue;
                }
                if b == b';' {
                    self.params.next_param();
                    i += 1;
                    continue;
                }
            }
            if let Some(event) = self.advance(bytes[i]) {
                Self::dispatch(event, sink);
            }
            i += 1;
        }
    }

    /// Deliver the complete, valid non-ASCII UTF-8 at the start of `bytes`,
    /// up to [`Self::UTF8_RUN_MAX`] bytes, as one character run. Returns the
    /// bytes consumed: 0 when `bytes` does not start with a complete valid
    /// sequence, which the state machine then decodes byte by byte, producing
    /// U+FFFD exactly as before. The cap bounds the rescan after each invalid
    /// byte in binary output.
    fn print_utf8_run(bytes: &[u8], sink: &mut impl TerminalSink) -> usize {
        let window = &bytes[..bytes.len().min(Self::UTF8_RUN_MAX)];
        let end = window.iter().position(|&b| b < 0x80).unwrap_or(window.len());
        let valid = match std::str::from_utf8(&window[..end]) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&window[..error.valid_up_to()]).unwrap_or_default(),
        };
        let mut run = ['\0'; Self::UTF8_RUN_MAX];
        let mut len = 0;
        for ch in valid.chars() {
            run[len] = ch;
            len += 1;
        }
        if len > 0 {
            sink.print_chars(&run[..len]);
        }
        valid.len()
    }

    const UTF8_RUN_MAX: usize = 64;

    /// Deliver one general-path event to a direct sink.
    fn dispatch(event: TermEvent<'_>, sink: &mut impl TerminalSink) {
        match event {
            TermEvent::Print(ch) => sink.print(ch),
            TermEvent::PrintRun(bytes) => sink.print_run(bytes),
            TermEvent::Control(control) => sink.control(control),
            TermEvent::Esc {
                intermediate,
                final_byte,
            } => sink.esc(intermediate, final_byte),
            TermEvent::Csi(action) => sink.csi(action),
            TermEvent::Osc(command) => sink.osc(command),
            TermEvent::Dcs(command) => sink.dcs(command),
            TermEvent::Ignored => sink.ignored(),
        }
    }

    /// Advance the state machine by one byte.
    ///
    /// Returns `Some(TermEvent)` when a complete sequence has been recognised,
    /// or `None` when more bytes are needed.
    #[inline]
    pub fn advance(&mut self, byte: u8) -> Option<TermEvent<'_>> {
        // ── Anywhere transitions (C1 controls, CAN, SUB, ESC) ────────────
        match byte {
            // CAN (0x18) and SUB (0x1A) abort any in-progress sequence.
            0x18 | 0x1A => {
                self.state = State::Ground;
                return Some(TermEvent::Control(C0::from_byte(byte)));
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

    fn ground(&mut self, byte: u8) -> Option<TermEvent<'static>> {
        match byte {
            // C0 controls
            0x00..=0x1F => Some(TermEvent::Control(C0::from_byte(byte))),
            // ASCII printable
            0x20..=0x7E => Some(TermEvent::Print(byte as char)),
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
            _ => Some(TermEvent::Print('\u{FFFD}')),
        }
    }

    // ── UTF-8 continuation ────────────────────────────────────────────────

    fn utf8_continue(
        &mut self,
        byte: u8,
        remaining: u8,
        acc: u32,
        total: u8,
    ) -> Option<TermEvent<'static>> {
        if byte & 0xC0 != 0x80 {
            // Not a continuation byte — replacement character, restart.
            self.state = State::Ground;
            return Some(TermEvent::Print('\u{FFFD}'));
        }
        let acc = (acc << 6) | u32::from(byte & 0x3F);
        if remaining == 1 {
            self.state = State::Ground;
            let ch = char::from_u32(acc).unwrap_or('\u{FFFD}');
            // Reject overlong encodings.
            let ok = match total {
                2 => acc >= 0x80,
                3 => acc >= 0x800,
                4 => (0x0001_0000..=0x0010_FFFF).contains(&acc),
                _ => false,
            };
            Some(TermEvent::Print(if ok { ch } else { '\u{FFFD}' }))
        } else {
            self.state = State::Utf8(remaining - 1, acc, total);
            None
        }
    }

    // ── ESC ───────────────────────────────────────────────────────────────

    const fn escape(&mut self, byte: u8) -> Option<TermEvent<'static>> {
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
                self.dcs_len = 0;
                self.in_dcs = true;
                self.state = State::DcsEntry;
                None
            }
            // SOS / PM / APC
            b'X' | b'^' | b'_' => {
                self.dcs_len = 0;
                self.in_dcs = false;
                self.state = State::SosPmApcString;
                None
            }
            // Final bytes (4/0–7/E)
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(TermEvent::Esc {
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

    const fn escape_intermediate(&mut self, byte: u8) -> Option<TermEvent<'static>> {
        match byte {
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(TermEvent::Esc {
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

    fn csi_entry(&mut self, byte: u8) -> Option<TermEvent<'static>> {
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
            // Colon (sub-parameter separator, e.g. `CSI 4:3 m` curly
            // underline or `CSI 38:2:r:g:b m` colon-form truecolor).
            b':' => {
                self.params.next_subparam();
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

    fn csi_param(&mut self, byte: u8) -> Option<TermEvent<'static>> {
        match byte {
            b'0'..=b'9' => {
                self.params.push_digit(byte);
                None
            }
            b';' => {
                self.params.next_param();
                None
            }
            b':' => {
                self.params.next_subparam();
                None
            }
            // Another intermediate — e.g. the SP in `CSI Ps SP q` (DECSCUSR).
            // BUG FIX: this previously changed state without recording the
            // byte, so `self.intermediate` stayed `None` and `dispatch_csi`
            // saw a bare `(None, b'q')` instead of `(Some(b' '), b'q')` —
            // DECSCUSR (and anything else with a numeric param before its
            // intermediate) could never be recognised.
            0x20..=0x2F => {
                self.intermediate = Some(byte);
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

    const fn csi_intermediate(&mut self, byte: u8) -> Option<TermEvent<'static>> {
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

    /// Narrow a raw CSI/SGR parameter (`u16`, since CSI params can carry
    /// values up to 65535 -- see `Params::get_raw`) to `u8` for the actions
    /// that only ever mean something in `0..=255` (erase-mode selectors,
    /// Kitty protocol flags, SGR color components). Out-of-range input is
    /// user-supplied CSI text, not an internal invariant violation -- masking
    /// to the low byte (matching plain `as u8`'s historical behavior) is the
    /// intentional, correct response, not an error to propagate.
    const fn truncate_param(v: u16) -> u8 {
        (v & 0xFF) as u8
    }

    const fn dispatch_csi(&mut self, final_byte: u8) -> TermEvent<'static> {
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
            (None, b'J') => CsiAction::EraseDisplay(Self::truncate_param(p.get_raw(0))),
            // EL
            (None, b'K') => CsiAction::EraseLine(Self::truncate_param(p.get_raw(0))),
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
            // REP
            (None, b'b') => CsiAction::RepeatChar(p.get_or(0, 1)),
            // HVP
            (None, b'f') => CsiAction::HorizVertPos {
                row: p.get_or(0, 1),
                col: p.get_or(1, 1),
            },
            // SGR
            (None, b'm') => CsiAction::Sgr(self.decode_sgr()),
            // DSR
            (None, b'n') => CsiAction::DeviceStatusReport(Self::truncate_param(p.get_raw(0))),
            // DA
            (None, b'c') => CsiAction::DeviceAttributes(Self::truncate_param(p.get_raw(0))),
            // DECSTBM
            (None, b'r') => CsiAction::SetScrollRegion {
                top: p.get_or(0, 1),
                bottom: p.get_or(1, u16::MAX),
            },
            // SM — multi-parameter (`CSI 4;20 h`), carry the whole list.
            (None, b'h') => CsiAction::SetMode(*p),
            // RM
            (None, b'l') => CsiAction::ResetMode(*p),
            // DECSET — multi-parameter: apps batch modes
            // (`CSI ?1002;1006 h`); taking only the first silently dropped
            // every other negotiation in the sequence.
            (Some(b'?'), b'h') => CsiAction::DecSet(*p),
            // DECRST
            (Some(b'?'), b'l') => CsiAction::DecRst(*p),
            // DECSCUSR — set cursor style (`CSI Ps SP q`).
            (Some(b' '), b'q') => CsiAction::CursorStyle(p.get_or(0, 1)),
            // TBC — tab clear (`CSI Ps g`).
            (None, b'g') => CsiAction::TabClear(p.get_raw(0)),
            // CHT — cursor forward tab.
            (None, b'I') => CsiAction::CursorForwardTab(p.get_or(0, 1)),
            // CBT — cursor backward tab.
            (None, b'Z') => CsiAction::CursorBackTab(p.get_or(0, 1)),
            // xterm modifyOtherKeys: `CSI > 4 ; Pv m`.
            (Some(b'>'), b'm') if p.get_raw(0) == 4 => {
                CsiAction::XtermModifyOtherKeys(p.get_raw(1))
            }
            // Kitty keyboard protocol.
            (Some(b'>'), b'u') => CsiAction::KittyKeyboardPush(Self::truncate_param(p.get_raw(0))),
            (Some(b'<'), b'u') => CsiAction::KittyKeyboardPop(p.get_or(0, 1)),
            (Some(b'='), b'u') => CsiAction::KittyKeyboardSet {
                flags: Self::truncate_param(p.get_raw(0)),
                mode: Self::truncate_param(p.get_or(1, 1)),
            },
            (Some(b'?'), b'u') => CsiAction::KittyKeyboardQuery,
            // Unknown
            _ => CsiAction::Unknown {
                final_byte,
                params: *p,
            },
        };

        self.reset_seq();
        TermEvent::Csi(action)
    }

    const fn decode_sgr(&self) -> SgrAttrs {
        let mut attrs = SgrAttrs::EMPTY;
        let params = &self.params;

        if params.is_empty() {
            attrs.push(SgrAttr::Reset);
            return attrs;
        }

        let mut idx = 0usize;
        while idx < params.len() {
            let val = params.get_raw(idx);
            let attr = match val {
                0 => SgrAttr::Reset,
                1 => SgrAttr::Bold,
                2 => SgrAttr::Dim,
                3 => SgrAttr::Italic,
                // `CSI 4 m` (bare) is always straight underline; `CSI 4:n m`
                // (colon sub-parameter) selects a style -- `is_sub(idx + 1)`
                // is what distinguishes it from `CSI 4;n m`, which is just
                // "underline, then attribute n" (two independent SGRs).
                4 if params.is_sub(idx + 1) => {
                    let style = Self::truncate_param(params.get_raw(idx + 1));
                    idx += 1;
                    SgrAttr::UnderlineStyle(style)
                }
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
                30..=37 => SgrAttr::FgStandard(Self::truncate_param(val - 30)),
                38 => match params.get_raw(idx + 1) {
                    5 => {
                        let color_idx = Self::truncate_param(params.get_raw(idx + 2));
                        idx += 2;
                        SgrAttr::Fg256(color_idx)
                    }
                    2 => {
                        let red = Self::truncate_param(params.get_raw(idx + 2));
                        let green = Self::truncate_param(params.get_raw(idx + 3));
                        let blue = Self::truncate_param(params.get_raw(idx + 4));
                        idx += 4;
                        SgrAttr::FgRgb(red, green, blue)
                    }
                    _ => SgrAttr::FgDefault,
                },
                39 => SgrAttr::FgDefault,
                40..=47 => SgrAttr::BgStandard(Self::truncate_param(val - 40)),
                48 => match params.get_raw(idx + 1) {
                    5 => {
                        let color_idx = Self::truncate_param(params.get_raw(idx + 2));
                        idx += 2;
                        SgrAttr::Bg256(color_idx)
                    }
                    2 => {
                        let red = Self::truncate_param(params.get_raw(idx + 2));
                        let green = Self::truncate_param(params.get_raw(idx + 3));
                        let blue = Self::truncate_param(params.get_raw(idx + 4));
                        idx += 4;
                        SgrAttr::BgRgb(red, green, blue)
                    }
                    _ => SgrAttr::BgDefault,
                },
                49 => SgrAttr::BgDefault,
                58 => match params.get_raw(idx + 1) {
                    5 => {
                        let color_idx = Self::truncate_param(params.get_raw(idx + 2));
                        idx += 2;
                        SgrAttr::UnderlineColor256(color_idx)
                    }
                    2 => {
                        let red = Self::truncate_param(params.get_raw(idx + 2));
                        let green = Self::truncate_param(params.get_raw(idx + 3));
                        let blue = Self::truncate_param(params.get_raw(idx + 4));
                        idx += 4;
                        SgrAttr::UnderlineColorRgb(red, green, blue)
                    }
                    _ => SgrAttr::UnderlineColorDefault,
                },
                59 => SgrAttr::UnderlineColorDefault,
                90..=97 => SgrAttr::FgStandard(Self::truncate_param(val - 90 + 8)),
                100..=107 => SgrAttr::BgStandard(Self::truncate_param(val - 100 + 8)),
                _ => {
                    idx += 1;
                    continue;
                }
            };
            attrs.push(attr);
            idx += 1;
        }
        attrs
    }

    // ── OSC ───────────────────────────────────────────────────────────────

    fn osc_string(&mut self, byte: u8) -> Option<TermEvent<'_>> {
        match byte {
            // BEL (xterm extension) or ST (ESC \, with ESC already consumed
            // by the anywhere rule so we see just the `\` here) terminates OSC.
            0x07 | 0x9C | b'\\' => {
                self.state = State::Ground;
                Some(self.dispatch_osc())
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

    fn dispatch_osc(&self) -> TermEvent<'_> {
        let data = &self.osc_buf[..self.osc_len];

        // Find the ';' separator between Ps and Pt.
        let sep = data.iter().position(|&b| b == b';');
        let (numeric_field, text_field) =
            sep.map_or_else(|| (data, &b""[..]), |pos| (&data[..pos], &data[pos + 1..]));

        let ps: u8 = numeric_field.iter().fold(0u8, |acc, &b| {
            if b.is_ascii_digit() {
                acc.saturating_mul(10).saturating_add(b - b'0')
            } else {
                // Non-numeric Ps is malformed; ignore the byte rather than
                // underflowing on `b - b'0'`.
                acc
            }
        });

        // Truncation is inherent to osc_buf's fixed OSC_BUF_MAX capacity —
        // no separate copy/allocation needed; these &str slices borrow
        // straight out of the parser's own stack buffer. A split may have
        // landed mid-codepoint (buffer cut a multi-byte UTF-8 sequence), so
        // fall back to the longest valid-UTF-8 prefix rather than losing
        // the whole string.
        let pt = core::str::from_utf8(text_field)
            .unwrap_or_else(|e| core::str::from_utf8(&text_field[..e.valid_up_to()]).unwrap_or(""));

        let cmd = match ps {
            0 | 2 => OscCmd::SetTitle(pt),
            8 => {
                // OSC 8 ; params ; uri BEL — hyperlink.
                let uri = pt.split_once(';').map_or(pt, |x| x.1);
                OscCmd::Hyperlink { uri }
            }
            4 => OscCmd::SetColor { pairs: pt },
            104 => OscCmd::ResetColor { ps: pt },
            10 => OscCmd::ForegroundColor(pt),
            11 => OscCmd::BackgroundColor(pt),
            52 => {
                // OSC 52 ; selection ; base64-data BEL — clipboard set/query.
                let mut parts = pt.splitn(2, ';');
                let selection = parts.next().unwrap_or("");
                let data = parts.next().unwrap_or("");
                OscCmd::Clipboard { selection, data }
            }
            133 => OscCmd::ShellIntegration(pt),
            _ => OscCmd::Unknown { ps, data: pt },
        };

        TermEvent::Osc(cmd)
    }

    // ── DCS / SOS / PM / APC ─────────────────────────────────────────────

    fn dcs_or_sos(&mut self, byte: u8) -> Option<TermEvent<'_>> {
        // ST (0x9C) or ESC \ terminates these sequences.
        if byte == 0x9C || byte == b'\\' {
            self.state = State::Ground;
            return Some(self.dispatch_dcs());
        }
        // Otherwise accumulate the payload (truncated past DCS_BUF_MAX --
        // DECRQSS requests and other DCS bodies we care about are short;
        // Sixel/terminfo-query payloads that overflow this just lose their
        // tail, matching the OSC buffer's truncation behavior).
        if self.dcs_len < DCS_BUF_MAX {
            self.dcs_buf[self.dcs_len] = byte;
            self.dcs_len += 1;
        }
        None
    }

    fn dispatch_dcs(&self) -> TermEvent<'_> {
        let data = core::str::from_utf8(&self.dcs_buf[..self.dcs_len]).unwrap_or_else(|e| {
            core::str::from_utf8(&self.dcs_buf[..e.valid_up_to()]).unwrap_or("")
        });
        if !self.in_dcs {
            // SOS/PM/APC: no request-string convention, always Unknown.
            return TermEvent::Dcs(DcsCmd::Unknown(data));
        }
        // DECRQSS: `$q<request>` -- e.g. "$qm" (SGR), "$q q" (DECSCUSR),
        // "$qr" (DECSTBM). Anything else falls through to Unknown.
        let cmd = data
            .strip_prefix("$q")
            .map_or(DcsCmd::Unknown(data), DcsCmd::RequestStatusString);
        TermEvent::Dcs(cmd)
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    const fn reset_seq(&mut self) {
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

    // Events now borrow from the parser's internal buffers (OSC text), so
    // tests assert inline inside the `feed` callback — mirroring how real
    // consumers (crates/grid's `Grid::apply`) are required to use them,
    // rather than collecting into an owned `Vec<TermEvent>`.

    #[test]
    fn ascii_text() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        p.feed(b"hello", |ev| {
            if let TermEvent::Print(c) = ev {
                seen.push(c);
            }
        });
        assert_eq!(seen, vec!['h', 'e', 'l', 'l', 'o']);
    }

    #[test]
    fn cursor_up() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[3A", |ev| {
            assert_eq!(ev, TermEvent::Csi(CsiAction::CursorUp(3)));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn cursor_position_default() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[H", |ev| {
            assert_eq!(
                ev,
                TermEvent::Csi(CsiAction::CursorPosition { row: 1, col: 1 })
            );
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn sgr_bold_fg_red() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[1;31m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                assert!(attrs.contains(&SgrAttr::Bold));
                assert!(attrs.contains(&SgrAttr::FgStandard(1)));
                count += 1;
            } else {
                panic!("expected SGR event, got {ev:?}");
            }
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn sgr_colon_underline_style_is_distinct_from_semicolon_form() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[4:3m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                assert!(attrs.contains(&SgrAttr::UnderlineStyle(3)));
                assert!(!attrs.contains(&SgrAttr::Underline));
                count += 1;
            } else {
                panic!("expected SGR event, got {ev:?}");
            }
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn sgr_semicolon_4_then_3_is_underline_then_italic_not_a_style() {
        // `CSI 4;3 m` -- two independent SGRs (underline, italic), NOT the
        // colon-form "underline style 3" -- must not be conflated.
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[4;3m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                assert!(attrs.contains(&SgrAttr::Underline));
                assert!(attrs.contains(&SgrAttr::Italic));
                assert!(!attrs.contains(&SgrAttr::UnderlineStyle(3)));
                count += 1;
            } else {
                panic!("expected SGR event, got {ev:?}");
            }
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn sgr_underline_color_256_and_rgb_and_default() {
        let mut p = Parser::new();
        let mut seen: Vec<SgrAttr> = Vec::new();
        p.feed(b"\x1b[58;5;9m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                seen.extend(attrs.iter());
            }
        });
        assert_eq!(seen, vec![SgrAttr::UnderlineColor256(9)]);

        seen.clear();
        p.feed(b"\x1b[58;2;10;20;30m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                seen.extend(attrs.iter());
            }
        });
        assert_eq!(seen, vec![SgrAttr::UnderlineColorRgb(10, 20, 30)]);

        seen.clear();
        p.feed(b"\x1b[59m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                seen.extend(attrs.iter());
            }
        });
        assert_eq!(seen, vec![SgrAttr::UnderlineColorDefault]);
    }

    #[test]
    fn sgr_colon_form_truecolor_still_decodes() {
        // `CSI 38:2:r:g:b m` -- ITU-T colon form of truecolor, seen from
        // some emitters (e.g. some tmux configurations) alongside the more
        // common semicolon form. Since `decode_sgr` reads sub-fields via
        // `get_raw` regardless of which separator produced them, this must
        // decode identically to the semicolon form.
        let mut p = Parser::new();
        let mut seen: Vec<SgrAttr> = Vec::new();
        p.feed(b"\x1b[38:2:10:20:30m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                seen.extend(attrs.iter());
            }
        });
        assert_eq!(seen, vec![SgrAttr::FgRgb(10, 20, 30)]);
    }

    #[test]
    fn sgr_reset_empty() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[m", |ev| {
            if let TermEvent::Csi(CsiAction::Sgr(attrs)) = ev {
                assert!(attrs.contains(&SgrAttr::Reset));
                count += 1;
            } else {
                panic!("expected SGR reset, got {ev:?}");
            }
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn osc_title() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b]2;My Terminal\x07", |ev| {
            assert!(matches!(
                ev,
                TermEvent::Osc(OscCmd::SetTitle("My Terminal"))
            ));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn decrqss_sgr_request_recognised() {
        // Terminated by the bare C1 ST byte (0x9C) -- an actual ESC byte
        // would win the parser's "anywhere" rule and re-enter Escape state
        // before `dcs_or_sos` ever saw it (see
        // `dcs_sequence_terminated_by_esc_backslash_yields_esc_event_not_dcs`
        // in advance_coverage.rs).
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1bP$qm", |_| {});
        p.feed(&[0x9C], |ev| {
            assert_eq!(ev, TermEvent::Dcs(DcsCmd::RequestStatusString("m")));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn decrqss_decscusr_request_recognised() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1bP$q q", |_| {});
        p.feed(&[0x9C], |ev| {
            assert_eq!(ev, TermEvent::Dcs(DcsCmd::RequestStatusString(" q")));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn dcs_without_request_prefix_is_unknown() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1bPnot-a-request", |_| {});
        p.feed(&[0x9C], |ev| {
            assert_eq!(ev, TermEvent::Dcs(DcsCmd::Unknown("not-a-request")));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn sos_pm_apc_dcs_payload_is_always_unknown() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1bX$qm", |_| {});
        p.feed(&[0x9C], |ev| {
            // SOS/PM/APC share the accumulation logic but never treat their
            // payload as a DECRQSS request -- that convention is DCS-only.
            assert_eq!(ev, TermEvent::Dcs(DcsCmd::Unknown("$qm")));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn erase_display() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[2J", |ev| {
            assert_eq!(ev, TermEvent::Csi(CsiAction::EraseDisplay(2)));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn dec_set() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[?25h", |ev| {
            match ev {
                TermEvent::Csi(CsiAction::DecSet(params)) => {
                    assert_eq!(params.iter().collect::<Vec<_>>(), vec![25]);
                }
                other => panic!("expected DecSet, got {other:?}"),
            }
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn dec_set_multiple_params_carries_all_modes() {
        // Apps batch mode negotiations: `CSI ?1002;1006 h` (mouse drag
        // tracking + SGR encoding). All params must survive parsing — the
        // old single-param DecSet silently dropped everything after the
        // first, which broke vim's mouse under any batching client.
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[?1002;1006h", |ev| {
            match ev {
                TermEvent::Csi(CsiAction::DecSet(params)) => {
                    assert_eq!(params.iter().collect::<Vec<_>>(), vec![1002, 1006]);
                }
                other => panic!("expected DecSet, got {other:?}"),
            }
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn utf8_two_byte() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        p.feed("é".as_bytes(), |ev| {
            if let TermEvent::Print(c) = ev {
                seen.push(c);
            }
        });
        assert_eq!(seen, vec!['é']);
    }

    #[test]
    fn utf8_emoji() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        p.feed("🦀".as_bytes(), |ev| {
            if let TermEvent::Print(c) = ev {
                seen.push(c);
            }
        });
        assert_eq!(seen, vec!['🦀']);
    }

    /// Collects every printed character, from single prints and runs alike.
    #[derive(Default)]
    struct Printed(Vec<char>);

    impl TerminalSink for Printed {
        fn print(&mut self, ch: char) {
            self.0.push(ch);
        }
        fn print_run(&mut self, bytes: &[u8]) {
            self.0.extend(bytes.iter().map(|&b| char::from(b)));
        }
        fn control(&mut self, _: C0) {}
        fn esc(&mut self, _: Option<u8>, _: u8) {}
        fn csi(&mut self, _: CsiAction) {}
        fn osc(&mut self, _: OscCmd<'_>) {}
        fn dcs(&mut self, _: DcsCmd<'_>) {}
        fn ignored(&mut self) {}
    }

    /// The UTF-8 run fast path must print exactly what byte-by-byte
    /// decoding prints, wherever a chunk boundary falls.
    #[test]
    fn utf8_runs_match_bytewise_decoding_across_splits() {
        let mut input = "é中\u{1f980}Ж".repeat(30).into_bytes();
        input.extend_from_slice(b"\xff\xe4\xb8a\xc3\xa9\xed\xa0\x80\xc0\xaf\xf0\x9f\x98");
        input.extend_from_slice("中".as_bytes());
        let mut bytewise = Vec::new();
        let mut p = Parser::new();
        for &b in &input {
            if let Some(TermEvent::Print(c)) = p.advance(b) {
                bytewise.push(c);
            }
        }
        for split in 0..input.len() {
            let mut p = Parser::new();
            let mut printed = Printed::default();
            p.feed_to(&input[..split], &mut printed);
            p.feed_to(&input[split..], &mut printed);
            assert_eq!(printed.0, bytewise, "split at {split}");
        }
    }

    #[test]
    fn decscusr_cursor_style_recognised() {
        // Regression: `csi_param`'s intermediate-byte transition used to
        // drop the SP byte, so `CSI Ps SP q` dispatched as bare `(None, 'q')`
        // and was never recognised at all.
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[2 q", |ev| {
            assert_eq!(ev, TermEvent::Csi(CsiAction::CursorStyle(2)));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn decscusr_default_param_is_one() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[ q", |ev| {
            assert_eq!(ev, TermEvent::Csi(CsiAction::CursorStyle(1)));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn tab_clear_and_tab_stop_moves() {
        let mut p = Parser::new();
        let mut i = 0;
        p.feed(b"\x1b[3g\x1b[2I\x1b[Z", |ev| {
            match i {
                0 => assert_eq!(ev, TermEvent::Csi(CsiAction::TabClear(3))),
                1 => assert_eq!(ev, TermEvent::Csi(CsiAction::CursorForwardTab(2))),
                2 => assert_eq!(ev, TermEvent::Csi(CsiAction::CursorBackTab(1))),
                _ => panic!("unexpected extra event"),
            }
            i += 1;
        });
        assert_eq!(i, 3);
    }

    #[test]
    fn repeat_char_default_and_explicit_count() {
        let mut p = Parser::new();
        let mut i = 0;
        p.feed(b"\x1b[b\x1b[5b", |ev| {
            match i {
                0 => assert_eq!(ev, TermEvent::Csi(CsiAction::RepeatChar(1))),
                1 => assert_eq!(ev, TermEvent::Csi(CsiAction::RepeatChar(5))),
                _ => panic!("unexpected extra event"),
            }
            i += 1;
        });
        assert_eq!(i, 2);
    }

    #[test]
    fn xterm_modify_other_keys_level() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x1b[>4;2m", |ev| {
            assert_eq!(ev, TermEvent::Csi(CsiAction::XtermModifyOtherKeys(2)));
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn kitty_keyboard_push_pop_set_query() {
        let mut p = Parser::new();
        let mut i = 0;
        p.feed(b"\x1b[>1u\x1b[<1u\x1b[=5;1u\x1b[?u", |ev| {
            match i {
                0 => assert_eq!(ev, TermEvent::Csi(CsiAction::KittyKeyboardPush(1))),
                1 => assert_eq!(ev, TermEvent::Csi(CsiAction::KittyKeyboardPop(1))),
                2 => assert_eq!(
                    ev,
                    TermEvent::Csi(CsiAction::KittyKeyboardSet { flags: 5, mode: 1 })
                ),
                3 => assert_eq!(ev, TermEvent::Csi(CsiAction::KittyKeyboardQuery)),
                _ => panic!("unexpected extra event"),
            }
            i += 1;
        });
        assert_eq!(i, 4);
    }

    #[test]
    fn osc_10_11_52_133_recognised() {
        let mut p = Parser::new();
        let mut seen = Vec::new();
        p.feed(b"\x1b]10;?\x07", |ev| seen.push(format!("{ev:?}")));
        p.feed(b"\x1b]11;?\x07", |ev| seen.push(format!("{ev:?}")));
        p.feed(b"\x1b]52;c;aGVsbG8=\x07", |ev| seen.push(format!("{ev:?}")));
        p.feed(b"\x1b]133;A\x07", |ev| seen.push(format!("{ev:?}")));
        assert!(seen[0].contains("ForegroundColor"));
        assert!(seen[1].contains("BackgroundColor"));
        assert!(seen[2].contains("Clipboard"));
        assert!(seen[3].contains("ShellIntegration"));
    }

    #[test]
    fn osc_4_set_color_recognised() {
        let mut p = Parser::new();
        let mut seen = None;
        p.feed(b"\x1b]4;1;#ff0000\x07", |ev| seen = Some(format!("{ev:?}")));
        let seen = seen.unwrap();
        assert!(seen.contains("SetColor"));
        assert!(seen.contains("1;#ff0000"));
    }

    #[test]
    fn osc_4_multi_pair_and_query_recognised() {
        let mut p = Parser::new();
        let mut seen = None;
        p.feed(b"\x1b]4;0;?;1;?\x07", |ev| seen = Some(format!("{ev:?}")));
        let seen = seen.unwrap();
        assert!(seen.contains("SetColor"));
        assert!(seen.contains("0;?;1;?"));
    }

    #[test]
    fn osc_104_reset_color_recognised() {
        let mut p = Parser::new();
        let mut seen = None;
        p.feed(b"\x1b]104;5\x07", |ev| seen = Some(format!("{ev:?}")));
        let seen = seen.unwrap();
        assert!(seen.contains("ResetColor"));
        assert!(seen.contains('5'));
    }

    #[test]
    fn osc_104_bare_reset_all_recognised() {
        let mut p = Parser::new();
        let mut seen = None;
        p.feed(b"\x1b]104\x07", |ev| seen = Some(format!("{ev:?}")));
        assert!(seen.unwrap().contains("ResetColor"));
    }

    #[test]
    fn c0_bell() {
        let mut p = Parser::new();
        let mut count = 0;
        p.feed(b"\x07", |ev| {
            assert_eq!(ev, TermEvent::Control(C0::Bell));
            count += 1;
        });
        assert_eq!(count, 1);
    }
}
