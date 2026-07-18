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
    pub(crate) const fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::Null,
            0x07 => Self::Bell,
            0x08 => Self::Backspace,
            0x09 => Self::Tab,
            0x0A..=0x0C => Self::LineFeed,
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
    /// Underline style (`CSI 4:n m` colon form only -- a bare `CSI 4 m` is
    /// [`Self::Underline`], always style 1/straight). `n`: 0 = none (same as
    /// `NoUnderline`), 1 = straight, 2 = double, 3 = curly, 4 = dotted,
    /// 5 = dashed. Any other value falls back to straight (1) rather than
    /// being dropped, matching how unknown SGR numeric codes elsewhere in
    /// this enum degrade gracefully instead of erroring.
    UnderlineStyle(u8),
    /// Underline color, standard/256-index form (`CSI 58;5;n m`).
    UnderlineColor256(u8),
    /// Underline color, 24-bit RGB form (`CSI 58;2;r;g;b m`).
    UnderlineColorRgb(u8, u8, u8),
    /// Underline color reset to the glyph's own foreground (`CSI 59 m`).
    UnderlineColorDefault,
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

    pub(crate) const fn push(&mut self, attr: SgrAttr) {
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
    /// REP — repeat the preceding graphic character `Ps` times (`CSI Ps b`).
    RepeatChar(u16),
    /// DECSCUSR — set cursor style (`CSI Ps SP q`).
    CursorStyle(u16),
    /// TBC — tab clear (`CSI Ps g`). `0` = clear stop at cursor, `3` = clear all.
    TabClear(u16),
    /// CHT — cursor forward tab (`CSI Ps I`): advance `Ps` tab stops.
    CursorForwardTab(u16),
    /// CBT — cursor backward tab (`CSI Ps Z`): retreat `Ps` tab stops.
    CursorBackTab(u16),
    /// `CSI > 4 ; Pv m` — xterm modifyOtherKeys level (`Pv`: 0/1/2).
    XtermModifyOtherKeys(u16),
    /// Kitty keyboard protocol: `CSI > flags u` — push a new entry onto the
    /// progressive-enhancement flag stack.
    KittyKeyboardPush(u8),
    /// Kitty keyboard protocol: `CSI < Pn u` — pop `Pn` entries off the stack.
    KittyKeyboardPop(u16),
    /// Kitty keyboard protocol: `CSI = flags ; mode u` — set the current
    /// stack entry. `mode`: 1 = replace, 2 = OR in, 3 = AND out (unset bits).
    KittyKeyboardSet { flags: u8, mode: u8 },
    /// Kitty keyboard protocol: `CSI ? u` — query the current flags (reply
    /// `CSI ? flags u`).
    KittyKeyboardQuery,
    /// Unknown CSI sequence.
    Unknown { final_byte: u8, params: Params },
}

/// Parsed OSC command.
///
/// Text payloads borrow from the parser's internal OSC accumulation buffer,
/// so an `OscCmd` (and the `TermEvent` wrapping it) must be consumed before the
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
    /// OSC 10 — query/set the default foreground color. `?` asks the
    /// terminal to reply with its current color (used by TUIs to detect
    /// light/dark theme); any other payload is a set request.
    ForegroundColor(&'a str),
    /// OSC 11 — query/set the default background color. Same shape as
    /// [`Self::ForegroundColor`].
    BackgroundColor(&'a str),
    /// OSC 52 — clipboard set/query. `selection` is the buffer letter
    /// (`c` = clipboard, `p`/`q` = primary/secondary, ...); `data` is the
    /// base64 payload (or `?` for a query, which we don't answer -- reading
    /// our own clipboard back out is not implemented).
    Clipboard { selection: &'a str, data: &'a str },
    /// OSC 133 — shell-integration semantic prompt marks (`A`=prompt start,
    /// `B`=command start, `C`=command executed, `D`=command finished).
    ShellIntegration(&'a str),
    /// Unrecognised OSC.
    Unknown { ps: u8, data: &'a str },
}

/// Parsed DCS command. Payloads borrow from the parser's internal DCS
/// accumulation buffer, same lifetime discipline as [`OscCmd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcsCmd<'a> {
    /// DECRQSS (`DCS $ q <request> ST`) -- request status string. `request`
    /// is the CSI-final-byte(s) identifying which setting is being asked
    /// about (e.g. `"m"` for SGR, `"r"` for DECSTBM, `" q"` for DECSCUSR).
    /// The terminal must reply `DCS 1 $ r <value> <request> ST` (valid) or
    /// `DCS 0 $ r ST` (invalid/unsupported).
    RequestStatusString(&'a str),
    /// Any other DCS payload (Sixel graphics, terminfo queries via
    /// `DCS + q`, ...) -- not handled, but the raw bytes are still surfaced
    /// rather than silently discarded, so a caller that *does* want to look
    /// can.
    Unknown(&'a str),
}

/// A terminal event emitted by the parser. Borrows from the parser's
/// internal buffers (see [`OscCmd`]), so it's only valid for the duration of
/// the [`Parser::feed`] callback that produced it.
///
/// Named `TermEvent`, not the bare `Event` this used to be, because the
/// input pipeline has three unrelated event types in a chain — the Wayland
/// wire event (`platform::wire::WireEvent`), the reader→main-thread event
/// (`platform::event::RawEvent`), and this one, several stages later and
/// downstream of the PTY entirely — and a bare `Event` at each stage used
/// to force every call site to alias-import to tell them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent<'a> {
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
    /// A DCS sequence (APC/PM/SOS are folded in as `Unknown` -- they share
    /// the same accumulate-until-ST shape but have no handler yet).
    Dcs(DcsCmd<'a>),
    /// Parser encountered data it doesn't handle; bytes were consumed cleanly.
    Ignored,
}
