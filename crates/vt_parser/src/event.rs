#![allow(clippy::missing_const_for_fn, clippy::manual_range_patterns)]
//! Terminal event types emitted by the parser.

use crate::params::Params;

/// Maximum number of SGR attributes tracked in one `CSI … m` sequence.
/// Fixed-size stack array — no heap allocation on the hot path.
pub const MAX_SGR_ATTRS: usize = 16;

/// C0 control character (0x00–0x1F, excluding ESC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C0 {
    Null,
    /// BEL — ring the bell.
    Bell,
    /// BS — backspace.
    Backspace,
    /// HT — horizontal tab.
    Tab,
    /// LF — line feed (also used for VT and FF).
    LineFeed,
    /// CR — carriage return.
    CarriageReturn,
    /// SO — shift out (activate G1 charset).
    ShiftOut,
    /// SI — shift in (activate G0 charset).
    ShiftIn,
    /// Other C0 byte.
    Other(u8),
}

impl C0 {
    pub(crate) fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::Null,
            0x07 => Self::Bell,
            0x08 => Self::Backspace,
            0x09 => Self::Tab,
            0x0A | 0x0B | 0x0C => Self::LineFeed,
            0x0D => Self::CarriageReturn,
            0x0E => Self::ShiftOut,
            0x0F => Self::ShiftIn,
            other => Self::Other(other),
        }
    }
}

/// Select Graphic Rendition attributes (from CSI … m).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgrAttr {
    Reset,
    Bold,
    Dim,
    Italic,
    Underline,
    Blink,
    RapidBlink,
    Inverse,
    Invisible,
    Strikethrough,
    NormalIntensity,
    NoItalic,
    NoUnderline,
    NoBlink,
    NoInverse,
    NoInvisible,
    NoStrikethrough,
    /// Foreground color: standard 3/4-bit (30–37, 90–97).
    FgStandard(u8),
    /// Background color: standard 3/4-bit (40–47, 100–107).
    BgStandard(u8),
    /// Foreground default (39).
    FgDefault,
    /// Background default (49).
    BgDefault,
    /// Foreground 256-color (38;5;n).
    Fg256(u8),
    /// Background 256-color (48;5;n).
    Bg256(u8),
    /// Foreground 24-bit RGB (38;2;r;g;b).
    FgRgb(u8, u8, u8),
    /// Background 24-bit RGB (48;2;r;g;b).
    BgRgb(u8, u8, u8),
}

/// Fixed-capacity stack buffer of decoded SGR attributes for one `CSI … m`.
///
/// Overflow (more than [`MAX_SGR_ATTRS`] attrs in one sequence — pathological
/// input) silently drops the extras rather than growing the heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SgrAttrs {
    buf: [SgrAttr; MAX_SGR_ATTRS],
    len: usize,
}

impl SgrAttrs {
    pub(crate) const EMPTY: Self = Self {
        buf: [SgrAttr::Reset; MAX_SGR_ATTRS],
        len: 0,
    };

    pub(crate) fn push(&mut self, attr: SgrAttr) {
        if self.len < MAX_SGR_ATTRS {
            self.buf[self.len] = attr;
            self.len += 1;
        }
    }

    /// Attributes accumulated so far, in sequence order.
    #[must_use]
    pub fn as_slice(&self) -> &[SgrAttr] {
        &self.buf[..self.len]
    }

    /// Iterate over the attributes.
    pub fn iter(&self) -> impl Iterator<Item = &SgrAttr> {
        self.as_slice().iter()
    }

    /// True if the given attribute was seen in this sequence.
    #[must_use]
    pub fn contains(&self, attr: &SgrAttr) -> bool {
        self.as_slice().contains(attr)
    }
}

impl<'a> IntoIterator for &'a SgrAttrs {
    type Item = &'a SgrAttr;
    type IntoIter = std::slice::Iter<'a, SgrAttr>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Parsed CSI action (the final byte of a CSI sequence determines the action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsiAction {
    /// CUU — cursor up (A).
    CursorUp(u16),
    /// CUD — cursor down (B).
    CursorDown(u16),
    /// CUF — cursor forward (C).
    CursorForward(u16),
    /// CUB — cursor back (D).
    CursorBack(u16),
    /// CNL — cursor next line (E).
    CursorNextLine(u16),
    /// CPL — cursor previous line (F).
    CursorPrevLine(u16),
    /// CHA — cursor horizontal absolute (G).
    CursorHorizAbs(u16),
    /// CUP — cursor position (H).
    CursorPosition { row: u16, col: u16 },
    /// ED — erase in display (J).
    EraseDisplay(u8),
    /// EL — erase in line (K).
    EraseLine(u8),
    /// IL — insert lines (L).
    InsertLines(u16),
    /// DL — delete lines (M).
    DeleteLines(u16),
    /// DCH — delete characters (P).
    DeleteChars(u16),
    /// ECH — erase characters (X).
    EraseChars(u16),
    /// SU — scroll up (S).
    ScrollUp(u16),
    /// SD — scroll down (T).
    ScrollDown(u16),
    /// HVP — horizontal vertical position (f, alias of CUP).
    HorizVertPos { row: u16, col: u16 },
    /// SGR — select graphic rendition (m).
    Sgr(SgrAttrs),
    /// DSR — device status report (n).
    DeviceStatusReport(u8),
    /// DECSTBM — set top and bottom margins (r).
    SetScrollRegion { top: u16, bottom: u16 },
    /// SM — set mode (h).
    SetMode(u16),
    /// RM — reset mode (l).
    ResetMode(u16),
    /// DECSET — DEC private mode set (?h).
    DecSet(u16),
    /// DECRST — DEC private mode reset (?l).
    DecRst(u16),
    /// DA — device attributes (c).
    DeviceAttributes(u8),
    /// ICH — insert characters (@).
    InsertChars(u16),
    /// Unknown CSI sequence.
    Unknown { final_byte: u8, params: Params },
}

/// Parsed OSC command.
///
/// Text payloads borrow from the parser's internal OSC accumulation buffer,
/// so an `OscCmd` (and the `Event` wrapping it) must be consumed before the
/// next byte is fed to the parser — exactly what the `Parser::feed` callback
/// shape guarantees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscCmd<'a> {
    /// OSC 0 / 2 — set window title.
    SetTitle(&'a str),
    /// OSC 4 — set color.
    SetColor { index: u8, spec: &'a str },
    /// OSC 8 — hyperlink.
    Hyperlink { uri: &'a str },
    /// Unrecognised OSC.
    Unknown { ps: u8, data: &'a str },
}

/// A terminal event emitted by the parser. Borrows from the parser's
/// internal buffers (see [`OscCmd`]), so it's only valid for the duration of
/// the [`Parser::feed`] callback that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    /// A printable Unicode character (or a multi-byte sequence decoded to char).
    Print(char),
    /// C0 control character.
    Control(C0),
    /// ESC followed by a single character (ESC `c` etc.).
    Esc {
        intermediate: Option<u8>,
        final_byte: u8,
    },
    /// A fully parsed CSI sequence.
    Csi(CsiAction),
    /// A fully parsed OSC sequence.
    Osc(OscCmd<'a>),
    /// DCS / APC / PM / SOS — ignored for now, emitted as raw bytes.
    Dcs,
    /// Parser encountered data it doesn't handle; bytes were consumed cleanly.
    Ignored,
}
