#![allow(
    // `Grid`/`TermWindow`-shaped state machines naturally accumulate a
    // handful of independent bool flags (blink on/off, focused, dirty,
    // bracketed-paste, ...); forcing them into an enum soup would obscure
    // more than it clarifies for booleans that are genuinely independent.
    clippy::struct_excessive_bools
)]
//! Terminal screen model.
//!
//! Owns the 2-D cell grid, cursor state, scrollback buffer, alternate screen,
//! and a dirty-region tracker. The grid-thread is the sole writer; the
//! renderer-thread receives [`DirtySnapshot`]s via an SPSC ring.
//!
//! # Threading
//!
//! ```text
//! vt_parser-thread ──spsc TermEvents──► grid-thread (owns Grid)
//!                                             │
//!                                             └──spsc DirtySnapshot──► renderer-thread
//! ```

mod base64;
mod cell;
pub mod cell_store;
mod cursor;
mod dirty;
mod line_wrap;
mod palette;
mod screen;
mod selection;
mod stats;

pub use cell::{Attrs, Cell, Color, UnderlineStyle};
pub use cursor::{Cursor, CursorShape};
pub use dirty::{
    CursorInfo, DirtyRegion, DirtyRow, DirtyRowContent, DirtySnapshot, ScrollBand, SnapshotKind,
};
pub use palette::XTERM_256;
pub use screen::Screen;
pub use selection::{Point as SelPoint, Selection};
pub use stats::ScrollStats;

use input::{KeyboardMode, MouseMode};
use vt_parser::{C0, CsiAction, DcsCmd, OscCmd, SgrAttr, TermEvent, TerminalSink};

/// Scrollback limit (lines above the visible area).
pub const SCROLLBACK_MAX: usize = 10_000;

/// What kind of repaint the next [`Grid::take_dirty`] owes the renderer.
///
/// Mutually exclusive by construction: a viewport scroll repaints every
/// visible row from scratch, which makes any accumulated incremental scroll
/// band moot, so there is no state where both apply. Replaces a `bool`
/// (`viewport_dirty`) plus an `Option<(u16, u16, u16)>` (`scroll_accum`) that
/// used to live side by side on `Grid` and had to be reasoned about together
/// (setting the scroll band while a viewport repaint was pending was
/// harmless -- `take_dirty` discarded it unread -- but only obviously so by
/// reading that function).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRepaint {
    /// Incremental: `dirty` tracks exactly which rows changed. `Some(band)`
    /// when everything in `[band.top, band.bot]` also shifted up by
    /// `band.delta` rows since the last snapshot, letting the renderer
    /// pixel-shift that band and paint only the true dirty rows on top; `None`
    /// when nothing scrolled, or a batch mixed scrolls over different regions
    /// (the fast path is abandoned and the dirty rows are repainted directly).
    Incremental(Option<ScrollBand>),
    /// A full-frame repaint of the whole viewport is owed: every visible row
    /// changed because the view scrolled into/out of scrollback.
    Viewport,
}

impl Default for PendingRepaint {
    fn default() -> Self {
        Self::Incremental(None)
    }
}

impl PendingRepaint {
    /// Fold one content scroll of `[top, bot]` by `delta` into an
    /// `Incremental` scroll band. Multiple scrolls over the *same* region
    /// within a batch add up; a scroll over a *different* region abandons the
    /// fast path (`Some((0, 0, 0))`, the same sentinel `take_dirty` already
    /// collapses to `None`). A pending `Viewport` repaint is left untouched --
    /// it will discard any scroll band on the next `take_dirty` regardless.
    const fn accumulate_scroll(&mut self, top: u16, bot: u16, delta: u16) {
        if delta == 0 {
            return;
        }
        let Self::Incremental(band) = self else {
            return;
        };
        *band = Some(match *band {
            None => ScrollBand { top, bot, delta },
            Some(b) if b.top == top && b.bot == bot => ScrollBand {
                top,
                bot,
                delta: b.delta.saturating_add(delta),
            },
            Some(_) => ScrollBand {
                top: 0,
                bot: 0,
                delta: 0,
            },
        });
    }
}

/// The terminal grid: primary screen, alternate screen, scrollback, cursor.
///
/// Feed [`Event`]s from the VT parser via [`Grid::apply`]. After each batch,
/// call [`Grid::take_dirty`] to get a [`DirtySnapshot`] for the renderer.
pub struct Grid {
    /// The active (primary) screen.
    primary: Screen,
    /// The alternate screen (activated by `smcup` / `?1049h`).
    alternate: Screen,
    /// True while the alternate screen is active.
    alt_active: bool,
    /// Bracketed paste mode (DECSET 2004). When enabled by the application,
    /// clipboard paste is wrapped in ESC[200~ … ESC[201~ so the app can tell
    /// pasted newlines apart from typed Enter (fish/zsh/neovim rely on this to
    /// not execute multi-line pastes).
    bracketed_paste: bool,
    /// Current cursor attributes (color, shape).
    cursor: Cursor,
    /// Accumulated SGR attributes for new cells.
    attrs: Attrs,
    /// Dirty region accumulator.
    dirty: DirtyRegion,
    /// Saved cursor (DECSC).
    saved_cursor: Option<(u16, u16, Attrs)>,
    /// Viewport scroll offset, in lines, into scrollback. `0` = live view at
    /// the bottom (normal terminal behavior); `k` = view scrolled `k` lines up
    /// into history. Only the primary screen has scrollback, so this is forced
    /// back to `0` whenever the alternate screen is active.
    viewport_offset: usize,
    /// What the next [`Grid::take_dirty`] owes the renderer: either an
    /// incremental repaint (tracked by `dirty`, with an optional accumulated
    /// scroll band for the pixel-shift fast path) or a full-frame viewport
    /// repaint (every visible row changed because the view scrolled). These
    /// two used to be a `bool` and an `Option<(u16, u16, u16)>` living side by
    /// side, which let callers set the scroll band while a viewport repaint
    /// was already pending -- harmless (it was discarded unread) but not
    /// obviously so. Modeling them as one enum makes "a viewport repaint
    /// makes any pending scroll band moot" a structural fact instead of
    /// something you have to trace through `take_dirty` to confirm.
    pending_repaint: PendingRepaint,
    /// Lines the primary screen's content scrolled up since the last
    /// [`Grid::take_dirty`] snapshot (new output pushing lines into
    /// scrollback). Emitted on the snapshot so the renderer can shift its
    /// selection/mirror to keep highlights glued to the text, then reset.
    content_delta: u16,
    /// Bytes the terminal must write back to the PTY (CPR, DA, etc.).
    /// Drained by the driver via [`Grid::take_replies`] after each batch.
    replies: Vec<u8>,
    /// Deferred-wrap ("pending wrap" / DEC wrapnext) state. Set when a glyph
    /// fills the last column: the cursor visually stays put, and the wrap is
    /// only realized when the *next* printable char arrives. A CR/LF/cursor
    /// move clears it instead — so a newline ending a full-width line leaves
    /// that line correctly *unwrapped* (no false stitch on resize).
    pending_wrap: bool,
    /// The last graphic character printed via [`Grid::print`] (post charset
    /// translation), for REP (`CSI Ps b`) to repeat. Cleared to `None` by
    /// any control/escape sequence that isn't itself a repeat, matching
    /// xterm: REP only repeats a character that was the *immediately*
    /// preceding thing printed.
    last_printed: Option<char>,
    /// The cursor info last handed out in a [`DirtySnapshot`] -- compared
    /// against the live cursor in [`Grid::take_dirty`] so a snapshot's
    /// `cursor_changed` flag is set whenever the cursor moved or toggled
    /// visibility, even on a frame where no cell content changed.
    last_cursor: CursorInfo,
    /// Set by [`Grid::requeue_snapshot`] when a snapshot carrying a cursor
    /// change couldn't be delivered (the ring was full). Forces the *next*
    /// [`Grid::take_dirty`] to report `cursor_changed` even if the cursor
    /// happens to already equal `last_cursor` (which was optimistically
    /// updated when the lost snapshot was taken).
    force_cursor_changed: bool,
    /// The viewport offset the renderer's backbuffer currently shows, used to
    /// compute the signed whole-screen pixel-shift for a scroll-back step.
    /// `Some(0)` after any incremental (live-bottom) frame; `Some(k)` after a
    /// viewport frame at offset `k`; `None` when it can't be trusted (a
    /// viewport frame was dropped by a full ring, so the next one must repaint
    /// in full rather than shift from a frame that never landed).
    ///
    /// Set optimistically when a snapshot is *built*; rolled back to `None` by
    /// [`Grid::requeue_snapshot`] if that build turned out to be undeliverable
    /// -- exactly mirroring how `force_cursor_changed` guards `last_cursor`.
    rendered_offset: Option<usize>,
    /// Negotiated mouse-reporting mode (DECSET 1000/1002/1003 select the
    /// tracking granularity; 1006 layers SGR encoding on top and is tracked
    /// separately in `sgr_mouse` since xterm lets an app toggle it
    /// independently, in either order). Read by the UI thread (via
    /// [`Grid::mouse_mode`]) to decide whether/how to report pointer events
    /// down the PTY; the grid itself never touches the pointer.
    mouse_mode: MouseMode,
    /// `CSI ? 1006 h/l` — SGR mouse coordinate encoding, layered on top of
    /// whichever tracking mode `mouse_mode` holds.
    sgr_mouse: bool,
    /// Negotiated keyboard-encoding protocol (`Legacy` / `XtermModifyOtherKeys`
    /// / `Kitty`), set by xterm's modifyOtherKeys CSI or the Kitty
    /// progressive-enhancement push/pop/set CSIs. Read by the UI thread's
    /// `Encoder` (via [`Grid::keyboard_mode`]) so keys are encoded the way
    /// the running application actually asked for.
    keyboard_mode: KeyboardMode,
    /// Kitty progressive-enhancement flag stack. The top of the stack (last
    /// element) is the active flag set; `CSI > flags u` pushes, `CSI < Pn u`
    /// pops `Pn` entries. Empty stack = flags 0 (all enhancements off).
    kitty_flag_stack: Vec<u8>,
    /// DECCKM (`CSI ? 1 h/l`) — application-cursor mode: arrow keys send SS3
    /// instead of CSI. Read by the UI thread's `Encoder`.
    app_cursor: bool,
    /// DECPAM/DECPNM (`ESC =` / `ESC >`) — application-keypad mode.
    app_keypad: bool,
    /// DECAWM (`CSI ? 7 h/l`) — autowrap mode. On by default (xterm/VT100
    /// default); when off, a glyph printed in the last column overwrites it
    /// instead of wrapping to the next line.
    autowrap: bool,
    /// DECOM (`CSI ? 6 h/l`) — origin mode. When on, cursor addressing
    /// (CUP/HVP) is relative to the scroll region's top margin instead of
    /// the whole screen, and the cursor is clamped inside the scroll region.
    origin_mode: bool,
    /// Focus-event reporting (`CSI ? 1004 h/l`, xterm). When on, the
    /// terminal must send `CSI I` / `CSI O` on focus in/out -- tracked here
    /// so the UI thread knows whether to bother encoding+forwarding focus
    /// changes.
    focus_events: bool,
    /// Tab stops (HTS/TBC), stored as a sorted set of 0-indexed columns.
    /// Starts at the classic every-8-columns default; `CSI Ps I`/`Z` (CHT/
    /// CBT), HTS and TBC all operate on this instead of assuming a fixed
    /// stride.
    tab_stops: Vec<u16>,
    /// G0 charset, designated by `ESC ( X`.
    g0_charset: Charset,
    /// G1 charset, designated by `ESC ) X`.
    g1_charset: Charset,
    /// Which of G0/G1 is currently invoked: `false` = G0 (the power-on
    /// default), `true` = G1 (via SO, `C0::ShiftOut`). SI (`C0::ShiftIn`)
    /// switches back to G0.
    shifted_to_g1: bool,
    /// Set by a BEL (C0 0x07) since the last [`Grid::take_bell`]; cleared by
    /// it. The UI thread polls this (same pattern as `bracketed_paste`) to
    /// decide whether to flash/notify.
    bell_pending: bool,
    /// The most recent OSC 0/2 title the application set, if any, since the
    /// last [`Grid::take_title`]; cleared by it. `None` when nothing new has
    /// arrived (the UI thread's window keeps showing whatever title it last
    /// applied -- this only carries the *change*, not a live mirror).
    pending_title: Option<String>,
    /// OSC 8 hyperlink table: `link_table[id - 1]` is the URI for
    /// `Cell::link_id == id` (id `0` means "no link", so this is 1-indexed).
    /// Append-only for the lifetime of the terminal (URIs are typically a
    /// tiny, bounded set -- shell prompts, `ls --hyperlink`, a handful of
    /// links in a man page -- so there's no eviction; a process living long
    /// enough to make this a real memory concern would have far bigger
    /// scrollback-driven costs already dominating).
    link_table: Vec<String>,
    /// The link id new cells are stamped with, per `print()`. Set by an OSC 8
    /// with a non-empty URI (interning it into `link_table` if new); reset to
    /// `0` by a bare `OSC 8 ; ; ST` (the close-link form).
    active_link_id: u32,
    /// How many entries of `link_table` [`Grid::take_new_links`] has already
    /// handed to the UI thread; the next call only reports the suffix past
    /// this index.
    published_links: usize,
    /// A decoded OSC 52 clipboard write pending delivery to the UI thread's
    /// `wl_data_source` publish, if any since the last [`Grid::take_clipboard`].
    /// One-shot, same pattern as `pending_title`.
    pending_clipboard: Option<String>,
    /// The terminal's own default foreground/background, as configured by
    /// the driver at startup from the resolved theme (see
    /// [`Grid::set_default_colors`]). Used only to answer OSC 10/11 queries
    /// (`ESC ] 10 ; ? ST` etc.) -- TUIs poll these to detect a light vs. dark
    /// theme. Defaults to a dark theme's usual fg/bg so a reply is still
    /// sane even if the driver never calls the setter (tests, an
    /// unconfigured embedding, ...).
    default_fg: Color,
    default_bg: Color,
    /// OSC 4 palette overrides: `Some((r,g,b))` at index `n` means index `n`
    /// was remapped away from the built-in [`XTERM_256`] entry; `None` means
    /// still default. Boxed to a fixed 256-slot array (1KB) rather than a
    /// sparse map -- OSC 4 addresses the full `u8` index space and a plain
    /// array read/write is cheaper than a hashmap lookup for the vanishingly
    /// rare case this is even touched (colorscheme-switcher scripts, a
    /// handful of calls once at shell startup, never in the render hot path).
    palette_overrides: Box<[Option<(u8, u8, u8)>; 256]>,
    /// Indices of `palette_overrides` [`Grid::take_palette_changes`] hasn't
    /// yet reported to the UI thread, in the order they were written. Same
    /// one-shot-drain contract as `pending_clipboard`/`take_new_links`: OSC 4
    /// changes must repaint the whole screen (any cell using that index looks
    /// different now), so the UI thread needs to *see* every change, not just
    /// the latest value.
    pending_palette_changes: Vec<u8>,
}

/// A designated character set slot (G0/G1). Only the two xterm/VT100
/// actually ships (ASCII and DEC Special Graphics -- box-drawing/line art)
/// are modeled; any other designation (UK, etc.) falls back to ASCII since
/// we don't have distinct glyphs for them anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Charset {
    #[default]
    Ascii,
    /// DEC Special Graphics (`ESC ( 0`): remaps a handful of ASCII bytes
    /// (`j`-`~` and a few others) to line-drawing/box glyphs, the classic
    /// `ncurses`/`vim`/`tmux` border-drawing trick predating Unicode box
    /// characters.
    DecSpecialGraphics,
}

impl Charset {
    /// Map a printed character through this charset. Only
    /// `DecSpecialGraphics` does anything; `Ascii` (and any charset we don't
    /// model) passes everything through unchanged.
    fn translate(self, ch: char) -> char {
        if self != Self::DecSpecialGraphics {
            return ch;
        }
        Self::DEC_SPECIAL_GRAPHICS_TABLE
            .iter()
            .find_map(|&(from, to)| (from == ch).then_some(to))
            .unwrap_or(ch)
    }

    /// (plain ASCII byte, DEC Special Graphics glyph) rows for
    /// [`Self::translate`]. A fixed fact table (which ASCII byte the VT100
    /// line-drawing charset remaps to which box/symbol glyph) rather than a
    /// 31-arm match -- see [`ComposeWithMark`]'s table just below for the
    /// same rationale applied to combining-mark composition.
    const DEC_SPECIAL_GRAPHICS_TABLE: &[(char, char)] = &[
        ('`', '\u{25c6}'), // diamond
        ('a', '\u{2592}'), // checkerboard
        ('b', '\u{2409}'), // HT symbol
        ('c', '\u{240c}'), // FF symbol
        ('d', '\u{240d}'), // CR symbol
        ('e', '\u{240a}'), // LF symbol
        ('f', '\u{00b0}'), // degree
        ('g', '\u{00b1}'), // plus/minus
        ('h', '\u{2424}'), // NL symbol
        ('i', '\u{240b}'), // VT symbol
        ('j', '\u{2518}'), // bottom-right corner
        ('k', '\u{2510}'), // top-right corner
        ('l', '\u{250c}'), // top-left corner
        ('m', '\u{2514}'), // bottom-left corner
        ('n', '\u{253c}'), // crossing lines
        ('o', '\u{23ba}'), // scan line 1
        ('p', '\u{23bb}'), // scan line 3
        ('q', '\u{2500}'), // horizontal line
        ('r', '\u{23bc}'), // scan line 7
        ('s', '\u{23bd}'), // scan line 9
        ('t', '\u{251c}'), // left tee
        ('u', '\u{2524}'), // right tee
        ('v', '\u{2534}'), // bottom tee
        ('w', '\u{252c}'), // top tee
        ('x', '\u{2502}'), // vertical line
        ('y', '\u{2264}'), // less-or-equal
        ('z', '\u{2265}'), // greater-or-equal
        ('{', '\u{03c0}'), // pi
        ('|', '\u{2260}'), // not-equal
        ('}', '\u{00a3}'), // pound sterling
        ('~', '\u{00b7}'), // centered dot
    ];
}

/// Extends `char` with precomposed-accent composition, as a method instead
/// of a free `compose_precomposed(base, mark)` function -- `base` (the
/// receiver) is the natural owner since this is fundamentally "what does
/// this char become when a mark lands on it".
trait ComposeWithMark {
    /// Compose this base character with a following zero-width combining
    /// mark into its precomposed (NFC) form, covering the combining marks a
    /// real keyboard or IME actually produces over a Latin base letter
    /// (acute, grave, circumflex, tilde, diaeresis, ring above, cedilla).
    /// Returns `None` for any pair not in the table -- most emoji ZWJ
    /// sequences, combining marks over non-Latin letters, or marks stacked
    /// more than once (this only handles a single mark following a plain
    /// base, matching what `Cell`, with no cluster storage, can represent at
    /// all).
    ///
    /// Deliberately a lookup table, not a call into a Unicode normalization
    /// crate: this crate has zero external dependencies beyond its sibling
    /// crates, and the realistic terminal input (typed accents, not
    /// arbitrary decomposed text) only ever exercises this small, fixed set.
    fn compose_with(self, mark: char) -> Option<char>;
}

impl ComposeWithMark for char {
    fn compose_with(self, mark: char) -> Option<char> {
        // A plain data table, not a giant match: every real keyboard/IME
        // combining-mark pair this crate handles is a fact, not a branch --
        // encoding them as `match` arms only inflated cyclomatic complexity
        // (56 arms = 56 paths for a tool to worry could each behave
        // differently) for something that's actually one code path (a
        // lookup) exercised over dozens of rows of data. A linear scan is
        // fine: the table is small and this only runs once per typed
        // combining mark, nowhere near a hot path.
        COMPOSE_TABLE
            .iter()
            .find_map(|&(base, m, composed)| (base == self && m == mark).then_some(composed))
    }
}

/// (base, combining mark, precomposed NFC form) rows for
/// [`ComposeWithMark::compose_with`]. See that method's doc comment for
/// what this table covers and why it's deliberately small.
const COMPOSE_TABLE: &[(char, char, char)] = &[
    ('a', '\u{301}', '\u{e1}'),
    ('a', '\u{300}', '\u{e0}'),
    ('a', '\u{302}', '\u{e2}'),
    ('a', '\u{303}', '\u{e3}'),
    ('a', '\u{308}', '\u{e4}'),
    ('a', '\u{30a}', '\u{e5}'),
    ('A', '\u{301}', '\u{c1}'),
    ('A', '\u{300}', '\u{c0}'),
    ('A', '\u{302}', '\u{c2}'),
    ('A', '\u{303}', '\u{c3}'),
    ('A', '\u{308}', '\u{c4}'),
    ('A', '\u{30a}', '\u{c5}'),
    ('e', '\u{301}', '\u{e9}'),
    ('e', '\u{300}', '\u{e8}'),
    ('e', '\u{302}', '\u{ea}'),
    ('e', '\u{308}', '\u{eb}'),
    ('E', '\u{301}', '\u{c9}'),
    ('E', '\u{300}', '\u{c8}'),
    ('E', '\u{302}', '\u{ca}'),
    ('E', '\u{308}', '\u{cb}'),
    ('i', '\u{301}', '\u{ed}'),
    ('i', '\u{300}', '\u{ec}'),
    ('i', '\u{302}', '\u{ee}'),
    ('i', '\u{308}', '\u{ef}'),
    ('I', '\u{301}', '\u{cd}'),
    ('I', '\u{300}', '\u{cc}'),
    ('I', '\u{302}', '\u{ce}'),
    ('I', '\u{308}', '\u{cf}'),
    ('o', '\u{301}', '\u{f3}'),
    ('o', '\u{300}', '\u{f2}'),
    ('o', '\u{302}', '\u{f4}'),
    ('o', '\u{303}', '\u{f5}'),
    ('o', '\u{308}', '\u{f6}'),
    ('O', '\u{301}', '\u{d3}'),
    ('O', '\u{300}', '\u{d2}'),
    ('O', '\u{302}', '\u{d4}'),
    ('O', '\u{303}', '\u{d5}'),
    ('O', '\u{308}', '\u{d6}'),
    ('u', '\u{301}', '\u{fa}'),
    ('u', '\u{300}', '\u{f9}'),
    ('u', '\u{302}', '\u{fb}'),
    ('u', '\u{308}', '\u{fc}'),
    ('U', '\u{301}', '\u{da}'),
    ('U', '\u{300}', '\u{d9}'),
    ('U', '\u{302}', '\u{db}'),
    ('U', '\u{308}', '\u{dc}'),
    ('n', '\u{303}', '\u{f1}'),
    ('N', '\u{303}', '\u{d1}'),
    ('c', '\u{327}', '\u{e7}'),
    ('C', '\u{327}', '\u{c7}'),
    ('y', '\u{301}', '\u{fd}'),
    ('y', '\u{308}', '\u{ff}'),
    ('Y', '\u{301}', '\u{dd}'),
    ('Y', '\u{308}', '\u{178}'),
];

impl Grid {
    /// Create a new grid with the given dimensions.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            primary: Screen::new(cols, rows, SCROLLBACK_MAX),
            alternate: Screen::new(cols, rows, 0),
            alt_active: false,
            bracketed_paste: false,
            cursor: Cursor::default(),
            attrs: Attrs::default(),
            dirty: DirtyRegion::new(cols, rows),
            saved_cursor: None,
            viewport_offset: 0,
            rendered_offset: Some(0),
            pending_repaint: PendingRepaint::default(),
            content_delta: 0,
            replies: Vec::new(),
            pending_wrap: false,
            last_printed: None,
            last_cursor: CursorInfo::default(),
            force_cursor_changed: false,
            mouse_mode: MouseMode::Off,
            sgr_mouse: false,
            keyboard_mode: KeyboardMode::Legacy,
            kitty_flag_stack: Vec::new(),
            app_cursor: false,
            app_keypad: false,
            autowrap: true,
            origin_mode: false,
            focus_events: false,
            tab_stops: Self::default_tab_stops(cols),
            g0_charset: Charset::Ascii,
            g1_charset: Charset::Ascii,
            shifted_to_g1: false,
            bell_pending: false,
            pending_title: None,
            link_table: Vec::new(),
            active_link_id: 0,
            published_links: 0,
            pending_clipboard: None,
            default_fg: Color::Rgb(0xcc, 0xcc, 0xcc),
            default_bg: Color::Rgb(0x1e, 0x1e, 0x2e),
            palette_overrides: Box::new([None; 256]),
            pending_palette_changes: Vec::new(),
        }
    }

    /// Set the default fg/bg colors OSC 10/11 replies report, from the
    /// driver's resolved theme. Call once after construction (the theme is
    /// fixed for the terminal's lifetime; there's no live-reload path yet).
    pub const fn set_default_colors(&mut self, fg: Color, bg: Color) {
        self.default_fg = fg;
        self.default_bg = bg;
    }

    /// Scroll the viewport by `delta` lines (positive = up into scrollback,
    /// negative = down toward the live bottom). Clamped to the available
    /// scrollback. This is a *view* operation only — it never moves the cursor
    /// or mutates cell data. No-op while the alternate screen is active (it has
    /// no scrollback).
    pub fn scroll_viewport(&mut self, delta: i32) {
        if self.alt_active {
            return;
        }
        // scrollback_len/viewport_offset are bounded by SCROLLBACK_MAX (a
        // small constant, far under i32::MAX), so these always fit.
        let max = i32::try_from(self.primary.scrollback_len()).unwrap_or(i32::MAX);
        let cur = i32::try_from(self.viewport_offset).unwrap_or(i32::MAX);
        let new = (cur + delta).clamp(0, max);
        if new != cur {
            // `new` is clamped to `0..=max` above, so it's always non-negative.
            self.viewport_offset = new.cast_unsigned() as usize;
            self.pending_repaint = PendingRepaint::Viewport;
        }
    }

    /// Current viewport offset (lines scrolled into scrollback; `0` = bottom).
    #[must_use]
    pub const fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Resize the grid to new dimensions, reflowing soft-wrapped lines to the
    /// new width (the primary screen) and clamping the cursor.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Clamp to at least 1x1: a compositor can send a 0-cell configure when
        // dragged to nothing, and a zero stride overflows every `cols - 1`.
        let cols = cols.max(1);
        let rows = rows.max(1);
        // When a deferred wrap is armed the cursor visually rests on the last
        // column but logically wants the next cell (offset = col + 1). Pass
        // that logical position so reflow lands the cursor at the start of the
        // continuation row rather than back on the final glyph.
        let cur_col = if self.pending_wrap {
            self.cursor.col + 1
        } else {
            self.cursor.col
        };
        let cur = (cur_col, self.cursor.row);
        let (new_col, new_row) = self.primary.resize(cols, rows, cur);
        // The alternate screen is a fixed fullscreen canvas (no scrollback, no
        // soft-wrap semantics), so it reflows trivially against the same cursor.
        self.alternate.resize(cols, rows, cur);

        if self.alt_active {
            self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
            self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
        } else {
            self.cursor.row = new_row;
            self.cursor.col = new_col;
        }

        self.dirty = DirtyRegion::new(cols, rows);
        self.dirty.mark_all();

        // Tab stops: drop any beyond the new (narrower) width; when
        // widening, extend with the default every-8-columns stride past
        // whatever the old width was (matches xterm: a widen doesn't
        // retroactively add stops inside the old visible area that TBC may
        // have cleared, only fills in the newly-revealed columns).
        let old_max = self.tab_stops.last().copied().map_or(0, |c| c + 1);
        self.tab_stops.retain(|&c| c < cols);
        if cols > old_max {
            self.tab_stops
                .extend((old_max.max(8).next_multiple_of(8)..cols).step_by(8));
        }
    }

    /// Apply a parsed VT event to the grid.
    ///
    /// New output snaps the viewport back to the live bottom (standard terminal
    /// behavior: typing or fresh output always returns you to the prompt).
    pub fn apply(&mut self, event: &TermEvent<'_>) {
        self.snap_viewport_to_bottom();
        match event {
            TermEvent::Print(ch) => self.print(*ch),
            TermEvent::PrintRun(bytes) => self.print_ascii_run(bytes),
            TermEvent::Control(c0) => self.control(*c0),
            TermEvent::Csi(action) => self.csi(action),
            TermEvent::Osc(cmd) => self.osc(cmd),
            TermEvent::Esc {
                intermediate,
                final_byte,
            } => {
                self.esc(*intermediate, *final_byte);
            }
            TermEvent::Dcs(cmd) => self.dcs(cmd),
            TermEvent::Ignored => {}
        }
    }

    /// Build a full-frame snapshot of the whole visible screen through the
    /// current viewport offset (`0` = live view at the bottom; `k` = scrolled
    /// `k` lines into scrollback). Used when the viewport moves: every row's
    /// content changes, so the renderer needs a complete repaint rather than
    /// incremental dirty rows.
    ///
    /// Lives on `Grid` (not `DirtySnapshot`) on purpose: `DirtySnapshot` is a
    /// render-transport DTO sent across the SPSC ring, so it must not depend on
    /// the live [`Screen`] model. `Grid` already owns both, so it's the natural
    /// place to bridge them -- keeping the dependency edge pointing one way
    /// (`Grid` -> {`Screen`, `DirtySnapshot`}) instead of inverting it.
    fn viewport_snapshot(&self) -> DirtySnapshot {
        let cols = self.primary.cols();
        let rows = self.primary.rows();
        let cur = self.viewport_offset;
        // We deliberately carry the *full* row set even when a pixel-shift is
        // possible: it keeps `compose`/`requeue_snapshot` trivial (a dropped
        // frame just re-marks every row) and makes the renderer's shm re-copy
        // and overlay repaint correct for free, since `touched_rows` already
        // spans the screen. The `shift` hint only lets the renderer skip the
        // expensive part -- rasterizing the surviving interior rows' glyphs.
        let mut out = Vec::with_capacity(usize::from(rows));
        for row in 0..rows {
            let cells: Vec<Cell> = (0..cols)
                .map(|col| self.primary.get_viewport_cell(col, row, cur))
                .collect();
            out.push(DirtyRow {
                row,
                content: DirtyRowContent::Cells(cells),
                soft_wrap: self.primary.viewport_soft_wrap(row, cur),
            });
        }
        // Signed whole-screen pixel-shift for the common small wheel/page step.
        // Emitted only when (1) the previously-rendered frame is trusted
        // (`Some`), (2) this is a *pure* view move -- new output means the
        // buffer itself changed, so shifting the old frame wouldn't reproduce
        // it (`content_delta == 0`), and (3) the step is small enough that some
        // of the screen survives the shift (`|s| < rows`); a larger jump has
        // nothing to reuse, so a full repaint is emitted instead.
        //
        // Sign convention (see `SnapshotKind::Viewport`): negative = view moved
        // up into history (reveal at the top); positive = toward the bottom
        // (reveal at the bottom). Scrolling up into history *raises* the
        // offset, so `shift = prev - cur`.
        let shift = match self.rendered_offset {
            Some(prev) if self.content_delta == 0 => {
                // viewport_offset is bounded by SCROLLBACK_MAX (a small
                // constant, far under i64::MAX), so these always fit.
                let prev = i64::try_from(prev).unwrap_or(i64::MAX);
                let cur_i64 = i64::try_from(cur).unwrap_or(i64::MAX);
                let s = prev - cur_i64;
                (s != 0 && s.unsigned_abs() < u64::from(rows))
                    .then(|| i32::try_from(s).unwrap_or(if s > 0 { i32::MAX } else { i32::MIN }))
            }
            _ => None,
        };
        DirtySnapshot {
            rows: out,
            cols,
            cursor: CursorInfo::default(),
            cursor_changed: false,
            content_delta: 0,
            kind: SnapshotKind::Viewport {
                offset: u16::try_from(cur.min(usize::from(u16::MAX))).unwrap_or(u16::MAX),
                shift,
            },
        }
    }

    /// Take the current dirty snapshot, resetting the dirty accumulator.
    ///
    /// Always fills in the cursor's current position/visibility, and sets
    /// `cursor_changed` whenever it differs from the cursor in the last
    /// snapshot taken -- so the renderer sees cursor movement even on a
    /// frame where no cell content changed (a bare cursor-move escape with
    /// no glyph printed), which it needs to animate the cursor sliding
    /// between cells.
    pub fn take_dirty(&mut self) -> DirtySnapshot {
        // The viewport can only scroll while the alt screen (with no
        // scrollback) is inactive, so its cursor is always the primary
        // screen's -- but while scrolled, the *visual* cursor position isn't
        // meaningful (it may be far off the visible viewport), so we hide it
        // rather than draw it at a wrong or nonsensical spot.
        let cur = if self.viewport_offset != 0 {
            CursorInfo {
                col: self.cursor.col,
                row: self.cursor.row,
                visible: false,
            }
        } else {
            CursorInfo {
                col: self.cursor.col,
                row: self.cursor.row,
                visible: self.cursor.visible,
            }
        };
        let cursor_changed = cur != self.last_cursor || self.force_cursor_changed;
        self.force_cursor_changed = false;
        self.last_cursor = cur;

        if self.pending_repaint == PendingRepaint::Viewport {
            self.pending_repaint = PendingRepaint::default();
            self.dirty.clear();
            let mut snap = self.viewport_snapshot();
            snap.cursor = cur;
            snap.cursor_changed = cursor_changed;
            // Keep the accumulated content_delta so the renderer can tell an
            // output-driven snap-to-bottom (offset -> 0 WITH new content,
            // which must NOT shift the selection by the view delta) from a
            // genuine user wheel-scroll (which must). For a pure wheel-scroll
            // there's no content delta, so this is 0 there anyway.
            snap.content_delta = self.content_delta;
            self.content_delta = 0;
            // Optimistically record what the renderer's backbuffer will show
            // once this frame lands, so the *next* scroll step can compute its
            // shift against it. Rolled back to `None` in `requeue_snapshot` if
            // the ring rejects this frame (shifting from a frame that never
            // arrived would tear).
            self.rendered_offset = Some(self.viewport_offset);
            // A viewport (full-frame) snapshot repaints everything, so any
            // accumulated scroll band -- there wasn't one anyway, since
            // `pending_repaint` can only hold one variant at a time -- is moot.
            return snap;
        }
        let screen = if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        };
        let mut snap = self.dirty.take_snapshot(screen);
        snap.cursor = cur;
        snap.cursor_changed = cursor_changed;
        snap.content_delta = self.content_delta;
        // Hand the renderer the exact scrolled band so it can pixel-shift then
        // paint the precise dirty rows on top. The (0,0,0) sentinel (mixed
        // regions) and any zero-delta collapse to None: no shift, just repaint
        // the dirty rows directly. `take_snapshot` already stamped this as
        // Incremental { scroll_span: None }; fill in the band.
        let PendingRepaint::Incremental(band) = core::mem::take(&mut self.pending_repaint) else {
            unreachable!("handled by the Viewport branch above");
        };
        snap.kind = SnapshotKind::Incremental {
            scroll_span: band.filter(|b| b.is_active()),
        };
        self.content_delta = 0;
        // An incremental frame is always the live bottom (the viewport can't be
        // scrolled while dirty rows accrue -- new output snaps it to 0 first),
        // so the backbuffer now shows offset 0. Record it as the baseline the
        // next scroll-back step shifts away from.
        self.rendered_offset = Some(0);
        snap
    }

    /// Merge a [`DirtySnapshot`] that could not be delivered (the renderer's
    /// ring was full) back into the dirty tracker, so its rows are re-emitted
    /// on the next [`Grid::take_dirty`] instead of being silently lost.
    ///
    /// Must be called immediately after the failed push, before any further
    /// [`Grid::apply`] runs on this thread: rows are re-marked dirty using
    /// their row index only (the cell contents underneath are still current,
    /// since nothing else ran between the `take_dirty` that produced this
    /// snapshot and this call). The cursor-changed flag and content-scroll
    /// delta — both consumed/reset by that `take_dirty` — are restored too.
    pub fn requeue_snapshot(&mut self, snap: &DirtySnapshot) {
        if let Some(offset) = snap.viewport_offset() {
            // A full-frame viewport snapshot: nothing incremental to
            // re-mark, just re-arm the viewport repaint so the next
            // take_dirty rebuilds the same full frame.
            self.viewport_offset = usize::from(offset);
            self.pending_repaint = PendingRepaint::Viewport;
            // This frame never reached the renderer, so its backbuffer still
            // shows whatever it did before -- which we can no longer name
            // (the optimistic update in take_dirty assumed delivery). Distrust
            // it: the re-armed frame will repaint in full (shift = None)
            // rather than pixel-shift from a frame that never landed.
            self.rendered_offset = None;
        } else {
            for row in &snap.rows {
                self.dirty.mark_row(row.row);
            }
        }
        self.content_delta = self.content_delta.saturating_add(snap.content_delta);
        // The dropped snapshot's pixel-shift never ran in the renderer's
        // backbuffer (it never arrived), and its precise dirty rows do NOT
        // include the shifted-but-unchanged rows -- those still rely on the
        // shift. So fold the scroll band back in; the next take_dirty re-emits
        // it (composing with any scroll since), and the renderer shifts then.
        if let Some(b) = snap.scroll_span() {
            self.pending_repaint
                .accumulate_scroll(b.top, b.bot, b.delta);
        }
        if snap.cursor_changed {
            self.force_cursor_changed = true;
        }
    }

    /// Return the viewport to the live bottom if it was scrolled up. Called on
    /// every applied event so fresh output always reveals the prompt.
    const fn snap_viewport_to_bottom(&mut self) {
        if self.viewport_offset != 0 {
            self.viewport_offset = 0;
            self.pending_repaint = PendingRepaint::Viewport;
        }
    }

    /// Take any pending reply bytes the terminal owes the host (responses to
    /// CPR/DSR/DA queries). The driver writes these back to the PTY. Empties
    /// the internal buffer.
    pub fn take_replies(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.replies)
    }

    /// Read-only view of the active screen (for testing / headless render).
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        self.active_screen()
    }

    /// Current cursor position.
    #[must_use]
    pub const fn cursor(&self) -> (u16, u16) {
        (self.cursor.col, self.cursor.row)
    }

    /// Current cursor visual shape (DECSCUSR, `CSI Ps SP q`).
    #[must_use]
    pub const fn cursor_shape(&self) -> CursorShape {
        self.cursor.shape
    }

    /// Read the primary screen's scroll-path counters (benchmark harness
    /// only; flood workloads scroll the primary screen).
    #[cfg(feature = "stats")]
    #[must_use]
    pub fn scroll_stats(&self) -> ScrollStats {
        self.primary.scroll_stats()
    }

    /// Zero the primary screen's scroll-path counters (benchmark harness
    /// only; e.g. between benchmark phases).
    #[cfg(feature = "stats")]
    pub fn reset_scroll_stats(&mut self) {
        self.primary.reset_scroll_stats();
    }

    // ── private ───────────────────────────────────────────────────────────

    const fn active_screen(&self) -> &Screen {
        if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    const fn active_screen_mut(&mut self) -> &mut Screen {
        if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn print_ascii_run(&mut self, bytes: &[u8]) {
        // DEC Special Graphics can translate printable ASCII into Unicode box
        // glyphs, so the direct narrow-cell path is valid only for the ASCII
        // charset. The fallback keeps the exact general-character semantics.
        let active_charset = if self.shifted_to_g1 {
            self.g1_charset
        } else {
            self.g0_charset
        };
        if active_charset != Charset::Ascii {
            for &byte in bytes {
                self.print(char::from(byte));
            }
            return;
        }

        // The parser has already established that every byte is printable
        // ASCII, hence every character is one cell wide. Keep the cursor and
        // row start in locals across the run: this removes the width-table
        // lookup, CellWidth trait dispatch, repeated screen selection, and
        // repeated physical-row arithmetic from the flood path. A deferred
        // wrap is still handled exactly at the boundary before the next glyph.
        let cols = self.active_screen().cols();
        let attrs = self.attrs;
        let link_id = self.active_link_id;
        let mut col = self.cursor.col;
        let mut row = self.cursor.row;

        let mut offset = 0;
        while offset < bytes.len() {
            if self.pending_wrap {
                self.active_screen_mut().set_wrapped(row, true);
                self.dirty.mark_row(row);
                col = 0;
                self.cursor.col = 0;
                self.cursor.row = row;
                self.cursor_down(1, true);
                row = self.cursor.row;
                self.pending_wrap = false;
            }

            let available = usize::from(cols - col);
            let count = (bytes.len() - offset).min(available);
            let written = self.active_screen_mut().write_ascii_cells(
                row,
                col,
                &bytes[offset..offset + count],
                attrs,
                link_id,
            );
            debug_assert_eq!(written, count);
            self.dirty.mark_rows(row, row);
            self.last_printed = Some(char::from(bytes[offset + count - 1]));
            offset += count;

            if offset < bytes.len() {
                col = cols - 1;
                self.cursor.col = col;
                self.pending_wrap = self.autowrap;
            } else if col + u16::try_from(count).unwrap_or(u16::MAX) < cols {
                col += u16::try_from(count).expect("ASCII run segment fits terminal width");
                self.cursor.col = col;
            } else {
                col = cols - 1;
                self.cursor.col = col;
                self.pending_wrap = self.autowrap;
            }
        }
    }

    fn print(&mut self, ch: char) {
        // Translate through whichever of G0/G1 is currently invoked (SO/SI)
        // before measuring width -- DEC Special Graphics remaps a handful of
        // plain ASCII bytes to box-drawing glyphs, which are still
        // single-width, so translating first doesn't change the width path
        // below.
        let active_charset = if self.shifted_to_g1 {
            self.g1_charset
        } else {
            self.g0_charset
        };
        let ch = active_charset.translate(ch);
        let width = {
            use text_layout::measurement::CellWidth as _;
            ch.cell_width()
        };
        if width == 0 {
            self.combine_onto_previous(ch);
            return;
        }

        if self.pending_wrap {
            let row = self.cursor.row;
            self.active_screen_mut().set_wrapped(row, true);
            self.dirty.mark_row(row);
            self.cursor.col = 0;
            self.cursor_down(1, true);
            self.pending_wrap = false;
        }

        // Cache geometry before the single mutable screen borrow below. This
        // keeps the ordinary printable path from repeatedly selecting the
        // primary/alternate screen and recalculating its visible index.
        let cols = self.active_screen().cols();
        if width == 2 && self.cursor.col + 1 == cols && self.autowrap {
            let row = self.cursor.row;
            let c = self.cursor.col;
            let screen = self.active_screen_mut();
            screen.set_cell(c, row, Cell::default());
            screen.set_wrapped(row, true);
            self.dirty.mark(c, row);
            self.cursor.col = 0;
            self.cursor_down(1, true);
        }

        self.last_printed = Some(ch);
        let col = self.cursor.col;
        let row = self.cursor.row;
        let attrs = self.attrs;
        let link_id = self.active_link_id;
        let is_wide = width == 2;
        {
            let lead = if is_wide {
                Cell::wide_lead(ch, attrs, link_id)
            } else {
                Cell::new(ch, attrs, link_id)
            };
            let spacer = (is_wide && col + 1 < cols).then(|| Cell::wide_spacer(attrs, link_id));
            self.active_screen_mut()
                .set_cell_pair(row, col, lead, spacer);
        }
        self.dirty.mark(col, row);
        if is_wide && col + 1 < cols {
            self.dirty.mark(col + 1, row);
        }

        let advance = if is_wide { 2 } else { 1 };
        if col + advance < cols {
            self.cursor.col += advance;
        } else {
            self.cursor.col = cols - 1;
            // DECAWM off: the cursor sticks at the last column instead of
            // arming a deferred wrap -- the next glyph overwrites it, rather
            // than wrapping to a new line.
            self.pending_wrap = self.autowrap;
        }
    }

    /// A zero-width combining mark arrived (e.g. typed as base letter plus
    /// U+0301 COMBINING ACUTE ACCENT, rather than a precomposed character).
    /// `Cell` has no grapheme-cluster storage, so full general composition
    /// isn't possible -- but the common case (a Latin base letter plus one
    /// of the handful of combining diacritics real keyboards/IMEs actually
    /// produce) is handled by rewriting the previous cell's glyph to its
    /// precomposed NFC form via [`ComposeWithMark::compose_with`]. Anything outside
    /// that table (emoji ZWJ sequences, combining marks over non-Latin base
    /// characters, ...) is dropped as before -- silently ignored rather than
    /// corrupting an unrelated cell, since we can't store the cluster.
    fn combine_onto_previous(&mut self, mark: char) {
        // Walk back over a wide glyph's spacer to find the actual base cell,
        // and don't step left past column 0 (a combining mark at the very
        // start of a line has nothing to combine with).
        if self.cursor.col == 0 {
            return;
        }
        let row = self.cursor.row;
        let c = self.cursor.col - 1;
        let screen = self.active_screen();
        let base_col = if screen.get_cell(c, row).spacer() && c > 0 {
            c - 1
        } else {
            c
        };
        let base = self.active_screen().get_cell(base_col, row);
        let Some(composed) = base.ch.compose_with(mark) else {
            return;
        };
        let mut cell = base;
        cell.ch = composed;
        self.active_screen_mut().set_cell(base_col, row, cell);
        self.dirty.mark(base_col, row);
    }

    fn control(&mut self, c0: C0) {
        // Any control byte other than the ones that are themselves no-ops
        // for cursor/character state breaks the "immediately preceding
        // print" chain REP relies on -- matches xterm, which only repeats
        // a character that was the very last thing printed.
        if !matches!(c0, C0::Bell | C0::Null | C0::Other(_)) {
            self.last_printed = None;
        }
        match c0 {
            C0::Bell => self.bell_pending = true,
            C0::Backspace => {
                self.pending_wrap = false;
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            C0::Tab => {
                // Advance to the next tab stop (HTS/TBC-aware; falls back to
                // the screen's right edge if there's no stop beyond the
                // cursor -- e.g. every stop past the cursor was TBC'd).
                self.pending_wrap = false;
                self.cursor_forward_tab(1);
            }
            C0::LineFeed => {
                // A newline after a full-width line: the deferred wrap is
                // abandoned, so the line stays unwrapped (no false stitch).
                self.pending_wrap = false;
                self.cursor_down(1, true);
            }
            C0::CarriageReturn => {
                self.pending_wrap = false;
                self.cursor.col = 0;
            }
            // SO/SI: invoke G1/G0 into the active charset slot printed
            // characters are translated through (see `Charset::translate`,
            // called from `print`).
            C0::ShiftOut => self.shifted_to_g1 = true,
            C0::ShiftIn => self.shifted_to_g1 = false,
            C0::Null | C0::Other(_) => {}
        }
    }

    fn cursor_down(&mut self, n: u16, scroll: bool) {
        let (rows, top, bot) = {
            let s = self.active_screen();
            (s.rows(), s.scroll_region().0, s.scroll_region().1)
        };
        if self.cursor.row + n > bot {
            if scroll {
                let scroll_lines = (self.cursor.row + n).saturating_sub(bot);
                let d = self.active_screen_mut().scroll_up(scroll_lines, top, bot);
                self.content_delta = self.content_delta.saturating_add(d);
                // Shift existing dirty marks up with the content (and dirty the
                // freshly-exposed bottom rows) rather than blanket-marking the
                // whole region -- so a batch mixing this scroll with in-place
                // edits reports each changed row at its true post-scroll
                // position. The clamped shift distance (matching the cells'
                // actual move) is recorded for the renderer's pixel-shift.
                // `bot >= top` here (a reversed region makes scroll_up return
                // 0), but guard the span arithmetic anyway.
                let region = bot.saturating_sub(top).saturating_add(1);
                let moved = scroll_lines.min(region);
                self.dirty.scroll_up(moved, top, bot);
                self.pending_repaint.accumulate_scroll(top, bot, moved);
                self.cursor.row = bot;
            } else {
                self.cursor.row = rows - 1;
            }
        } else {
            self.cursor.row += n;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn csi(&mut self, action: &CsiAction) {
        // Any cursor movement or content change abandons a deferred wrap. Pure
        // attribute changes (SGR) and host queries (DSR/DA) leave it intact so
        // a color change between the last glyph and the next still wraps.
        if !matches!(
            action,
            CsiAction::Sgr(_) | CsiAction::DeviceStatusReport(_) | CsiAction::DeviceAttributes(_)
        ) {
            self.pending_wrap = false;
        }
        // Same "only right after a print" rule as C0 controls -- everything
        // except REP itself (which reads it) and the same pure attribute/
        // query actions that leave pending_wrap alone.
        if !matches!(
            action,
            CsiAction::RepeatChar(_)
                | CsiAction::Sgr(_)
                | CsiAction::DeviceStatusReport(_)
                | CsiAction::DeviceAttributes(_)
        ) {
            self.last_printed = None;
        }
        match action {
            CsiAction::CursorUp(n) => {
                self.cursor.row = self.cursor.row.saturating_sub(*n);
            }
            CsiAction::CursorDown(n) => {
                let rows = self.active_screen().rows();
                self.cursor.row = self.cursor.row.saturating_add(*n).min(rows - 1);
            }
            CsiAction::CursorForward(n) => {
                let cols = self.active_screen().cols();
                self.cursor.col = self.cursor.col.saturating_add(*n).min(cols - 1);
            }
            CsiAction::CursorBack(n) => {
                self.cursor.col = self.cursor.col.saturating_sub(*n);
            }
            CsiAction::CursorNextLine(n) => {
                self.cursor.col = 0;
                let rows = self.active_screen().rows();
                self.cursor.row = self.cursor.row.saturating_add(*n).min(rows - 1);
            }
            CsiAction::CursorPrevLine(n) => {
                self.cursor.col = 0;
                self.cursor.row = self.cursor.row.saturating_sub(*n);
            }
            CsiAction::CursorHorizAbs(col) => {
                let cols = self.active_screen().cols();
                self.cursor.col = col.saturating_sub(1).min(cols - 1);
            }
            CsiAction::CursorPosition { row, col } | CsiAction::HorizVertPos { row, col } => {
                let cols = self.active_screen().cols();
                let rows = self.active_screen().rows();
                self.cursor.col = col.saturating_sub(1).min(cols - 1);
                // DECOM: row is relative to the scroll region's top margin
                // and clamped to the region's bottom, not the whole screen.
                if self.origin_mode {
                    let (top, bot) = self.active_screen().scroll_region();
                    self.cursor.row = top
                        .saturating_add(row.saturating_sub(1))
                        .min(bot)
                        .min(rows - 1);
                } else {
                    self.cursor.row = row.saturating_sub(1).min(rows - 1);
                }
            }
            CsiAction::EraseDisplay(mode) => {
                let col = self.cursor.col;
                let row = self.cursor.row;
                let screen = self.active_screen_mut();
                let (cols, rows) = (screen.cols(), screen.rows());
                match mode {
                    0 => {
                        // Erase from cursor to end of screen.
                        screen.erase_range(col, row, cols - 1, rows - 1);
                    }
                    1 => {
                        // Erase from start to cursor.
                        screen.erase_range(0, 0, col, row);
                    }
                    2 | 3 => {
                        // Erase entire display. This terminal also drops
                        // scrollback here (see erase_scrollback): ED 3 is
                        // xterm's explicit "erase scrollback", and we extend
                        // the same to ED 2 so a plain `clear` wipes history too
                        // (matching the user's expectation). Both force the
                        // viewport back to the live bottom.
                        //
                        // `clear()` does one bulk `fill()` over the whole
                        // visible area (+ a flat wrapped-flag reset), instead
                        // of `erase_range`'s per-row `erase_line_range` calls
                        // with their per-row wide-char boundary checks -- for
                        // a whole-screen erase there's no boundary to check
                        // (every row runs col 0..=cols-1), so the loop is pure
                        // overhead. This is the single hottest path in the
                        // alt-screen-thrash workload (smcup/rmcup + ED2 on
                        // every flip).
                        screen.clear();
                        screen.erase_scrollback();
                    }
                    _ => {}
                }
                self.dirty.mark_all();
            }
            CsiAction::EraseLine(mode) => {
                let col = self.cursor.col;
                let row = self.cursor.row;
                let screen = self.active_screen_mut();
                let cols = screen.cols();
                match mode {
                    0 => screen.erase_line_range(row, col, cols - 1),
                    1 => screen.erase_line_range(row, 0, col),
                    2 => screen.erase_line_range(row, 0, cols - 1),
                    _ => {}
                }
                self.dirty.mark_row(row);
            }
            CsiAction::InsertLines(n) => {
                let row = self.cursor.row;
                let (_, bot) = self.active_screen().scroll_region();
                self.active_screen_mut().insert_lines(row, *n, bot);
                self.dirty.mark_rows(row, bot);
            }
            CsiAction::DeleteLines(n) => {
                let row = self.cursor.row;
                let (_, bot) = self.active_screen().scroll_region();
                self.active_screen_mut().delete_lines(row, *n, bot);
                self.dirty.mark_rows(row, bot);
            }
            CsiAction::DeleteChars(n) => {
                let (col, row) = (self.cursor.col, self.cursor.row);
                self.active_screen_mut().delete_chars(col, row, *n);
                self.dirty.mark_row(row);
            }
            CsiAction::InsertChars(n) => {
                let (col, row) = (self.cursor.col, self.cursor.row);
                self.active_screen_mut().insert_chars(col, row, *n);
                self.dirty.mark_row(row);
            }
            CsiAction::RepeatChar(n) => {
                // REP -- repeat the last printed graphic character `n`
                // times, as if it had been sent again that many times. No
                // preceding character (freshly reset, or last thing was a
                // control/escape sequence) is a no-op, matching xterm.
                if let Some(ch) = self.last_printed {
                    for _ in 0..*n {
                        self.print(ch);
                    }
                }
            }
            CsiAction::EraseChars(n) => {
                let (col, row) = (self.cursor.col, self.cursor.row);
                let cols = self.active_screen().cols();
                let end = col.saturating_add(*n).min(cols);
                self.active_screen_mut().erase_line_range(row, col, end - 1);
                self.dirty.mark_row(row);
            }
            CsiAction::ScrollUp(n) => {
                let (top, bot) = self.active_screen().scroll_region();
                let d = self.active_screen_mut().scroll_up(*n, top, bot);
                self.content_delta = self.content_delta.saturating_add(d);
                let region = bot.saturating_sub(top).saturating_add(1);
                let moved = (*n).min(region);
                self.dirty.scroll_up(moved, top, bot);
                self.pending_repaint.accumulate_scroll(top, bot, moved);
            }
            CsiAction::ScrollDown(n) => {
                let (top, bot) = self.active_screen().scroll_region();
                self.active_screen_mut().scroll_down(*n, top, bot);
                self.dirty.mark_rows(top, bot);
            }
            CsiAction::SetScrollRegion { top, bottom } => {
                let rows = self.active_screen().rows();
                let t = top.saturating_sub(1).min(rows - 1);
                let b = bottom.saturating_sub(1).min(rows - 1);
                self.active_screen_mut().set_scroll_region(t, b);
                self.cursor.col = 0;
                self.cursor.row = 0;
            }
            CsiAction::Sgr(attrs) => {
                for attr in attrs {
                    self.apply_sgr(*attr);
                }
            }
            CsiAction::DeviceStatusReport(n) => match n {
                // DSR 5 — "report status": reply OK (CSI 0 n).
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                // DSR 6 — Cursor Position Report: reply CSI row ; col R (1-based).
                // fish and other shells block on this to track their prompt.
                6 => {
                    use std::io::Write as _;
                    let row = self.cursor.row + 1;
                    let col = self.cursor.col + 1;
                    let _ = write!(self.replies, "\x1b[{row};{col}R");
                }
                _ => {}
            },
            CsiAction::DeviceAttributes(_) => {
                // DA — Device Attributes. Reply as a VT102 ("\x1b[?6c") so apps
                // that probe capabilities before drawing get an answer.
                self.replies.extend_from_slice(b"\x1b[?6c");
            }
            // Multi-parameter: `CSI ?1002;1006 h` batches several mode
            // negotiations in one sequence — apply every one.
            CsiAction::DecSet(params) => {
                for mode in params.iter() {
                    self.set_dec_mode(mode, true);
                }
            }
            CsiAction::DecRst(params) => {
                for mode in params.iter() {
                    self.set_dec_mode(mode, false);
                }
            }
            CsiAction::SetMode(params) => {
                for mode in params.iter() {
                    Self::set_ansi_mode(mode, true);
                }
            }
            CsiAction::ResetMode(params) => {
                for mode in params.iter() {
                    Self::set_ansi_mode(mode, false);
                }
            }
            CsiAction::CursorStyle(style) => self.set_cursor_style(*style),
            CsiAction::TabClear(mode) => self.tab_clear(*mode),
            CsiAction::CursorForwardTab(n) => self.cursor_forward_tab(*n),
            CsiAction::CursorBackTab(n) => self.cursor_back_tab(*n),
            CsiAction::XtermModifyOtherKeys(level) => {
                // xterm modifyOtherKeys level 2 is the only level that
                // actually changes wire encoding (level 0 is off, level 1
                // only affects a handful of already-ambiguous keys we treat
                // the same as legacy) -- so anything >= 2 switches the
                // encoder over, and 0/1 fall back to Legacy.
                self.keyboard_mode = if *level >= 2 {
                    KeyboardMode::XtermModifyOtherKeys
                } else {
                    KeyboardMode::Legacy
                };
            }
            CsiAction::KittyKeyboardPush(flags) => {
                self.kitty_flag_stack.push(*flags);
                self.keyboard_mode = KeyboardMode::Kitty;
            }
            CsiAction::KittyKeyboardPop(n) => {
                let n = usize::from(*n).max(1);
                let new_len = self.kitty_flag_stack.len().saturating_sub(n);
                self.kitty_flag_stack.truncate(new_len);
                if self.kitty_flag_stack.is_empty() {
                    self.keyboard_mode = KeyboardMode::Legacy;
                }
            }
            CsiAction::KittyKeyboardSet { flags, mode } => {
                let top = self.kitty_flag_stack.last().copied().unwrap_or(0);
                let new_top = match mode {
                    2 => top | flags,
                    3 => top & !flags,
                    _ => *flags, // 1 = replace (and anything unrecognised)
                };
                if let Some(last) = self.kitty_flag_stack.last_mut() {
                    *last = new_top;
                } else {
                    self.kitty_flag_stack.push(new_top);
                }
                self.keyboard_mode = KeyboardMode::Kitty;
            }
            CsiAction::KittyKeyboardQuery => {
                use std::io::Write as _;
                let flags = self.kitty_flag_stack.last().copied().unwrap_or(0);
                let _ = write!(self.replies, "\x1b[?{flags}u");
            }
            // Unrecognised sequence: safely ignorable no-op.
            CsiAction::Unknown { .. } => {}
        }
    }

    fn apply_sgr(&mut self, attr: SgrAttr) {
        match attr {
            SgrAttr::Reset => self.attrs = Attrs::default(),
            SgrAttr::Bold => self.attrs.set_bold(true),
            SgrAttr::Dim => self.attrs.set_dim(true),
            SgrAttr::Italic => self.attrs.set_italic(true),
            SgrAttr::Underline => self.attrs.set_underline(true),
            SgrAttr::Blink | SgrAttr::RapidBlink => self.attrs.set_blink(true),
            SgrAttr::Inverse => self.attrs.set_inverse(true),
            SgrAttr::Invisible => self.attrs.set_invisible(true),
            SgrAttr::Strikethrough => self.attrs.set_strikethrough(true),
            SgrAttr::NormalIntensity => {
                self.attrs.set_bold(false);
                self.attrs.set_dim(false);
            }
            SgrAttr::NoItalic => self.attrs.set_italic(false),
            SgrAttr::NoUnderline => self.attrs.set_underline(false),
            SgrAttr::NoBlink => self.attrs.set_blink(false),
            SgrAttr::NoInverse => self.attrs.set_inverse(false),
            SgrAttr::NoInvisible => self.attrs.set_invisible(false),
            SgrAttr::NoStrikethrough => self.attrs.set_strikethrough(false),
            SgrAttr::FgStandard(n) | SgrAttr::Fg256(n) => self.attrs.fg = Color::Indexed(n),
            SgrAttr::BgStandard(n) | SgrAttr::Bg256(n) => self.attrs.bg = Color::Indexed(n),
            SgrAttr::FgDefault => self.attrs.fg = Color::Default,
            SgrAttr::BgDefault => self.attrs.bg = Color::Default,
            SgrAttr::FgRgb(r, g, b) => self.attrs.fg = Color::Rgb(r, g, b),
            SgrAttr::BgRgb(r, g, b) => self.attrs.bg = Color::Rgb(r, g, b),
            SgrAttr::UnderlineStyle(n) => {
                self.attrs
                    .set_underline_style(crate::cell::UnderlineStyle::from_sgr(n));
                // A style of 0 ("CSI 4:0 m") is the colon-form way to turn
                // underline off entirely -- everything else implies it's on
                // (mirrors how a bare `CSI 4 m` always turns it on).
                self.attrs.set_underline(n != 0);
            }
            SgrAttr::UnderlineColor256(n) => self.attrs.underline_color = Color::Indexed(n),
            SgrAttr::UnderlineColorRgb(r, g, b) => {
                self.attrs.underline_color = Color::Rgb(r, g, b);
            }
            SgrAttr::UnderlineColorDefault => self.attrs.underline_color = Color::Default,
        }
    }

    /// `CSI ? Pm h/l` — DEC private mode set/reset. Everything that isn't a
    /// plain ANSI `SM`/`RM` mode lives here: cursor visibility, bracketed
    /// paste, alt-screen, mouse tracking, app-cursor/keypad, autowrap,
    /// origin mode, and focus events.
    fn set_dec_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            // DECCKM — application cursor keys.
            1 => self.app_cursor = enable,
            // DECOM — origin mode: cursor addressing becomes relative to the
            // scroll region's top margin. Also re-homes the cursor to the
            // (new) origin, matching real terminals' DECOM behavior.
            6 => {
                self.origin_mode = enable;
                self.cursor.col = 0;
                self.cursor.row = if enable {
                    self.active_screen().scroll_region().0
                } else {
                    0
                };
            }
            // DECAWM — autowrap.
            7 => self.autowrap = enable,
            // DECTEM — cursor visibility
            25 => self.cursor.visible = enable,
            // Mouse tracking granularity: X10/Normal/ButtonMotion/AnyEvent.
            // Only one tracking mode is active at a time; the last one set
            // wins (matches xterm). Disabling one that isn't active is a
            // harmless no-op (checked so `?1000l` after `?1002h` doesn't
            // clobber the more specific mode a well-behaved app already
            // switched to — mirrors xterm's own behavior).
            1000 => self.set_mouse_tracking(enable, MouseMode::Normal),
            1002 => self.set_mouse_tracking(enable, MouseMode::ButtonMotion),
            1003 => self.set_mouse_tracking(enable, MouseMode::AnyEvent),
            9 => self.set_mouse_tracking(enable, MouseMode::X10),
            // SGR mouse coordinate encoding, layered on top of the tracking
            // mode above.
            1006 => self.sgr_mouse = enable,
            // Focus in/out event reporting.
            1004 => self.focus_events = enable,
            // Bracketed paste — the app wants pasted text wrapped in
            // ESC[200~ … ESC[201~ (read by the driver when pasting).
            2004 => self.bracketed_paste = enable,
            // DECALTBUF / smcup (1049)
            1049 | 47 | 1047 => {
                if enable && !self.alt_active {
                    self.saved_cursor = Some((self.cursor.col, self.cursor.row, self.attrs));
                    self.alternate.clear();
                    self.alt_active = true;
                    self.cursor.col = 0;
                    self.cursor.row = 0;
                    self.dirty.mark_all();
                } else if !enable && self.alt_active {
                    self.alt_active = false;
                    if let Some((col, row, attrs)) = self.saved_cursor.take() {
                        self.cursor.col = col;
                        self.cursor.row = row;
                        self.attrs = attrs;
                    }
                    self.dirty.mark_all();
                }
            }
            _ => {}
        }
    }

    /// Recompute `mouse_mode` after a tracking-granularity DECSET/DECRST.
    /// Enabling sets it to `which` outright (last-set-wins, matching
    /// xterm); disabling only clears it if `which` is the mode currently
    /// active, so e.g. `?1000l` after the app already switched to `?1002h`
    /// doesn't clobber the newer mode.
    fn set_mouse_tracking(&mut self, enable: bool, which: MouseMode) {
        if enable {
            self.mouse_mode = which;
        } else if self.mouse_mode == which {
            self.mouse_mode = MouseMode::Off;
        }
    }

    /// `CSI Pm h/l` — plain ANSI set/reset mode (no `?` prefix). Distinct
    /// mode space from DEC private modes (e.g. ANSI `4` is IRM/insert mode,
    /// unrelated to DEC private `4`). Nothing in this space is implemented
    /// yet -- IRM (insert/replace) has no effect on `Grid::print` -- so this
    /// is currently a documented no-op rather than a silent gap. Takes no
    /// `&self`/`&mut self`: it doesn't touch `Grid` state (yet), so it's an
    /// associated fn rather than a method that would trip `unused_self`.
    const fn set_ansi_mode(_mode: u16, _enable: bool) {}

    /// DECSCUSR (`CSI Ps SP q`) — set cursor shape/blink. `Ps`: 0/1 = blinking
    /// block, 2 = steady block, 3 = blinking underline, 4 = steady underline,
    /// 5 = blinking bar, 6 = steady bar. The renderer's own blink animation
    /// is driven by config (not per-app blink requests), so the odd/even
    /// blink-vs-steady distinction collapses to shape only.
    const fn set_cursor_style(&mut self, style: u16) {
        self.cursor.shape = match style {
            0..=2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => self.cursor.shape,
        };
    }

    /// The default tab stops: every 8 columns, matching classic VT100/xterm
    /// behavior before any HTS/TBC has run.
    fn default_tab_stops(cols: u16) -> Vec<u16> {
        (0..cols).step_by(8).collect()
    }

    /// HTS (`ESC H`) — set a tab stop at the cursor's current column.
    fn set_tab_stop(&mut self) {
        let col = self.cursor.col;
        if let Err(pos) = self.tab_stops.binary_search(&col) {
            self.tab_stops.insert(pos, col);
        }
    }

    /// TBC (`CSI Ps g`) — tab clear. `0` clears the stop at the cursor
    /// column (if any); `3` (and xterm's `5`) clears every stop.
    fn tab_clear(&mut self, mode: u16) {
        match mode {
            0 => {
                let col = self.cursor.col;
                if let Ok(pos) = self.tab_stops.binary_search(&col) {
                    self.tab_stops.remove(pos);
                }
            }
            3 | 5 => self.tab_stops.clear(),
            _ => {}
        }
    }

    /// CHT (`CSI Ps I`) — advance the cursor forward by `Ps` tab stops (at
    /// least 1), clamped to the last column.
    fn cursor_forward_tab(&mut self, n: u16) {
        let cols = self.active_screen().cols();
        let mut col = self.cursor.col;
        for _ in 0..n.max(1) {
            if let Some(&s) = self.tab_stops.iter().find(|&&s| s > col) {
                col = s;
            } else {
                col = cols - 1;
                break;
            }
        }
        self.cursor.col = col.min(cols - 1);
    }

    /// CBT (`CSI Ps Z`) — retreat the cursor backward by `Ps` tab stops (at
    /// least 1), clamped to column 0.
    fn cursor_back_tab(&mut self, n: u16) {
        let mut col = self.cursor.col;
        for _ in 0..n.max(1) {
            if let Some(&s) = self.tab_stops.iter().rev().find(|&&s| s < col) {
                col = s;
            } else {
                col = 0;
                break;
            }
        }
        self.cursor.col = col;
    }

    /// Whether the application has enabled bracketed paste (DECSET 2004).
    /// Read by the driver when wrapping clipboard paste in ESC[200~ … ESC[201~.
    #[must_use]
    pub const fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    /// Whether the alternate screen (DECSET 1049/47/1047) is active. Read by
    /// the UI thread to route the mouse wheel: on the alt screen (no
    /// scrollback) a wheel event with no app-negotiated mouse tracking is
    /// translated to arrow keys ("alternate scroll"), so full-screen apps
    /// like `less` or a pager inside nvim still scroll.
    #[must_use]
    pub const fn alt_screen_active(&self) -> bool {
        self.alt_active
    }

    /// The negotiated mouse tracking granularity (`Off`/`X10`/`Normal`/
    /// `ButtonMotion`/`AnyEvent`), independent of SGR encoding (see
    /// [`Grid::sgr_mouse`]). Read by the UI thread's `Encoder` to decide
    /// whether/which pointer events to forward to the PTY.
    #[must_use]
    pub const fn mouse_mode(&self) -> MouseMode {
        self.mouse_mode
    }

    /// Whether SGR mouse coordinate encoding (DECSET 1006) is active,
    /// layered independently on top of [`Grid::mouse_mode`]'s tracking
    /// granularity.
    #[must_use]
    pub const fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }

    /// The negotiated keyboard-encoding protocol (xterm modifyOtherKeys /
    /// Kitty progressive enhancement / legacy). Read by the UI thread's
    /// `Encoder`.
    #[must_use]
    pub const fn keyboard_mode(&self) -> KeyboardMode {
        self.keyboard_mode
    }

    /// The active Kitty progressive-enhancement flags (top of the negotiated
    /// stack), meaningful only while `keyboard_mode() == KeyboardMode::Kitty`.
    #[must_use]
    pub fn kitty_flags(&self) -> u8 {
        self.kitty_flag_stack.last().copied().unwrap_or(0)
    }

    /// DECCKM — application-cursor mode (arrow keys send SS3 instead of CSI).
    #[must_use]
    pub const fn app_cursor(&self) -> bool {
        self.app_cursor
    }

    /// DECPAM/DECPNM — application-keypad mode.
    #[must_use]
    pub const fn app_keypad(&self) -> bool {
        self.app_keypad
    }

    /// Whether the application has enabled focus-event reporting (DECSET
    /// 1004, xterm). Read by the UI thread to decide whether to encode and
    /// forward `wl_keyboard` enter/leave as `CSI I` / `CSI O`.
    #[must_use]
    pub const fn focus_events(&self) -> bool {
        self.focus_events
    }

    /// Whether a BEL (C0 0x07) has rung since the last call, clearing the
    /// flag. Polled by the UI thread (same pattern as `bracketed_paste`) to
    /// decide whether to flash/notify.
    pub fn take_bell(&mut self) -> bool {
        core::mem::take(&mut self.bell_pending)
    }

    /// The most recent OSC 0/2 window title the application set, if any
    /// since the last call, clearing it. `None` means no new title arrived
    /// (the UI thread's window keeps showing whatever it last applied).
    pub const fn take_title(&mut self) -> Option<String> {
        self.pending_title.take()
    }

    /// A decoded OSC 52 clipboard write the application made, if any since
    /// the last call, clearing it. Polled by the UI thread (same pattern as
    /// `take_title`) to publish the text via the platform's existing
    /// `wl_data_source` clipboard machinery.
    pub const fn take_clipboard(&mut self) -> Option<String> {
        self.pending_clipboard.take()
    }

    /// The effective color (override if OSC 4 set one, else the built-in
    /// `XTERM_256` default) for palette index `n`. What an OSC 4 query
    /// replies with, and what a driver should paint index `n` as -- so the
    /// renderer and the query-reply path can never disagree with each other.
    #[must_use]
    pub fn palette_color(&self, n: u8) -> (u8, u8, u8) {
        self.palette_overrides[n as usize].unwrap_or(XTERM_256[n as usize])
    }

    /// Palette indices `osc()` has changed (via OSC 4 or OSC 104) since the
    /// last call, draining the queue. The UI thread mirrors these into its
    /// renderer's theme and forces a full repaint -- any cell using one of
    /// these indices now paints differently.
    pub fn take_palette_changes(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.pending_palette_changes)
    }

    fn set_palette_override(&mut self, n: u8, rgb: (u8, u8, u8)) {
        self.palette_overrides[n as usize] = Some(rgb);
        self.pending_palette_changes.push(n);
    }

    fn reset_palette_override(&mut self, n: u8) {
        if self.palette_overrides[n as usize].take().is_some() {
            self.pending_palette_changes.push(n);
        }
    }

    fn reset_all_palette_overrides(&mut self) {
        for n in 0..256u16 {
            // `0..256u16` is always `<= 255`, so this always fits in u8.
            self.reset_palette_override(u8::try_from(n).expect("palette index fits in u8"));
        }
    }

    /// OSC 4 handler: `ESC ] 4 ; index ; spec [ ; index ; spec ... ] ST`.
    /// Each pair is either a query (a bare `?` spec, replies with the
    /// current color in the same `rgb:rrrr/gggg/bbbb` form OSC 10/11 use) or
    /// a set (any parseable color spec). A malformed pair (odd count,
    /// unparseable spec, out-of-range index text) is skipped -- matches how
    /// a set request for something else unsupported is handled elsewhere in
    /// this parser: best-effort, never an error reply.
    fn osc_set_color(&mut self, pairs: &str) {
        use std::io::Write as _;
        let mut parts = pairs.split(';');
        while let Some(idx_s) = parts.next() {
            let Some(spec) = parts.next() else { break };
            let Ok(idx) = idx_s.parse::<u16>() else {
                continue;
            };
            if idx > 255 {
                continue;
            }
            // Checked `<= 255` above, so this always fits in u8.
            let idx = u8::try_from(idx).expect("checked idx <= 255 fits in u8");
            if spec == "?" {
                let (r, g, b) = self.palette_color(idx);
                let _ = write!(
                    self.replies,
                    "\x1b]4;{idx};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
                    u16::from(r) * 0x101,
                    u16::from(g) * 0x101,
                    u16::from(b) * 0x101,
                );
            } else if let Some(rgb) = Color::parse_spec(spec) {
                self.set_palette_override(idx, rgb);
            }
        }
    }

    /// OSC 104 handler: `ESC ] 104 [ ; index [ ; index ... ] ] ST`. An empty
    /// Pt resets every overridden index; otherwise each `;`-separated index
    /// is reset individually (non-numeric/out-of-range entries are skipped,
    /// same best-effort policy as `osc_set_color`).
    fn osc_reset_color(&mut self, ps: &str) {
        if ps.is_empty() {
            self.reset_all_palette_overrides();
            return;
        }
        for idx_s in ps.split(';') {
            if let Ok(idx) = idx_s.parse::<u16>()
                && idx <= 255
            {
                // Checked `<= 255` above, so this always fits in u8.
                self.reset_palette_override(
                    u8::try_from(idx).expect("checked idx <= 255 fits in u8"),
                );
            }
        }
    }

    fn osc(&mut self, cmd: &OscCmd) {
        match cmd {
            OscCmd::SetTitle(title) => {
                self.pending_title = Some((*title).to_string());
            }
            OscCmd::Hyperlink { uri } => self.set_active_link(uri),
            OscCmd::Clipboard { selection, data } => self.set_clipboard(selection, data),
            OscCmd::ForegroundColor(query) => self.reply_default_color(10, query, self.default_fg),
            OscCmd::BackgroundColor(query) => self.reply_default_color(11, query, self.default_bg),
            OscCmd::SetColor { pairs } => self.osc_set_color(pairs),
            OscCmd::ResetColor { ps } => self.osc_reset_color(ps),
            _ => {}
        }
    }

    /// OSC 10/11 handler: `ESC ] <ps> ; ? ST` queries the terminal's default
    /// fg/bg, replying in the same OSC form the query arrived in
    /// (`ESC ] <ps> ; rgb:rrrr/gggg/bbbb ST`) -- what fzf, some vim
    /// colorschemes, and other light/dark-aware TUIs poll for at startup.
    /// Any payload other than a bare `?` is a *set* request (`ESC ] 10 ;
    /// <spec> ST`); we don't support changing the theme at runtime, so
    /// that's silently ignored rather than erroring -- matches how a set
    /// request for something else unsupported would be handled elsewhere in
    /// this parser.
    fn reply_default_color(&mut self, ps: u8, query: &str, color: Color) {
        use std::io::Write as _;
        if query != "?" {
            return;
        }
        let (r, g, b) = match color {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(_) | Color::Default => (0xcc, 0xcc, 0xcc),
        };
        // Each channel is reported as a 16-bit value (xterm convention);
        // an 8-bit channel `c` maps to `c * 0x101` so `0xff` -> `0xffff`
        // (exact endpoints) rather than a naive `<< 8` which would leave
        // `0xff00` short of white.
        let _ = write!(
            self.replies,
            "\x1b]{ps};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
            u16::from(r) * 0x101,
            u16::from(g) * 0x101,
            u16::from(b) * 0x101,
        );
    }

    /// OSC 52 handler: `ESC ] 52 ; <selection> ; <base64> ST`. Only a write to
    /// the system clipboard (`c`) or the combined primary+clipboard selector
    /// (`s`) is honoured -- `p`/`q` (primary/secondary X selections) have no
    /// Wayland equivalent worth modeling, and `data == "?"` (a read-back
    /// query) is deliberately not answered: echoing arbitrary clipboard
    /// contents back to *any* program that asks is the exact vector real
    /// terminals had to walk back after early over-eager OSC 52 support
    /// turned into a fingerprinting/exfiltration primitive. A blank/invalid
    /// payload is silently ignored rather than clearing the clipboard --
    /// matching xterm's own tolerance for malformed OSC 52.
    fn set_clipboard(&mut self, selection: &str, data: &str) {
        if data == "?" || !(selection.is_empty() || selection == "c" || selection == "s") {
            return;
        }
        if let Some(text) = base64::Base64::decode_to_string(data) {
            self.pending_clipboard = Some(text);
        }
    }

    /// OSC 8 handler: an empty `uri` closes the current link (subsequent
    /// cells go back to `link_id == 0`); a non-empty one opens a new one,
    /// interning it into `link_table` -- reusing an existing entry's id if
    /// this exact URI is already the most recently opened one, which is the
    /// overwhelmingly common case (`OSC 8 ; ; url ST text OSC 8 ; ; ST`
    /// wrapping a single run of text) and keeps that pattern from growing the
    /// table on every single link a prompt or `ls --hyperlink` prints.
    fn set_active_link(&mut self, uri: &str) {
        if uri.is_empty() {
            self.active_link_id = 0;
            return;
        }
        if self.link_table.last().is_some_and(|last| last == uri) {
            self.active_link_id = u32::try_from(self.link_table.len()).unwrap_or(u32::MAX);
            return;
        }
        // `Cell` stores link ids narrowed to u16 (see `Cell::link_id`), so
        // stop issuing new ids at that boundary: further distinct URIs print
        // as plain unlinked text instead of aliasing entry 65 535. 65k live
        // hyperlinks in one grid is far beyond any real session; degrading
        // gracefully beats widening every cell by 2 bytes for it.
        if self.link_table.len() >= usize::from(u16::MAX) {
            self.active_link_id = 0;
            return;
        }
        self.link_table.push(uri.to_string());
        self.active_link_id = u32::try_from(self.link_table.len()).unwrap_or(u32::MAX);
    }

    /// Resolve a [`Cell::link_id`] back to its URI, or `None` for `0`
    /// ("no link") or a stale id from before the last RIS. Read by the UI
    /// thread to decide what to open on a hyperlink click and whether to draw
    /// the hover-underline.
    #[must_use]
    pub fn link_uri(&self, link_id: u32) -> Option<&str> {
        if link_id == 0 {
            return None;
        }
        self.link_table
            .get(usize::try_from(link_id).ok()?.checked_sub(1)?)
            .map(String::as_str)
    }

    /// Newly-interned link URIs since the last call, as `(id, uri)` pairs, in
    /// the order they were interned; clears the backlog. The grid itself
    /// lives on the parser thread, so the UI thread (which needs the URI text
    /// to open on click) can't just borrow `link_table` directly -- this lets
    /// it mirror the table incrementally the same low-volume,
    /// observed-exactly-once way `take_bell`/`take_title` hand over one-shot
    /// events, without re-sending the whole table every batch.
    pub fn take_new_links(&mut self) -> Vec<(u32, String)> {
        let start = self.published_links;
        let out: Vec<(u32, String)> = self.link_table[start..]
            .iter()
            .enumerate()
            .map(|(i, uri)| {
                (
                    u32::try_from(start + i + 1).unwrap_or(u32::MAX),
                    uri.clone(),
                )
            })
            .collect();
        self.published_links = self.link_table.len();
        out
    }

    /// DECRQSS (`DCS $ q <request> ST`) -- request status string. Replies
    /// `DCS 1 $ r <value> <request> ST` for the handful of settings this
    /// terminal actually models (SGR, DECSTBM scroll region, DECSCUSR cursor
    /// style); anything else gets the "invalid request" form
    /// (`DCS 0 $ r ST`), matching real terminals rather than staying silent
    /// (silence reads as a hang to a well-behaved app polling for it).
    fn dcs(&mut self, cmd: &DcsCmd) {
        use std::io::Write as _;
        let DcsCmd::RequestStatusString(request) = cmd else {
            return;
        };
        let dcs_prefix = "\x1bP";
        let dcs_suffix = "\x1b\\";
        match *request {
            "m" => {
                let sgr = self.sgr_report_string();
                let _ = write!(self.replies, "{dcs_prefix}1$r{sgr}m{dcs_suffix}");
            }
            "r" => {
                let (top, bot) = self.active_screen().scroll_region();
                let _ = write!(
                    self.replies,
                    "{dcs_prefix}1$r{};{}r{dcs_suffix}",
                    top + 1,
                    bot + 1
                );
            }
            " q" => {
                let style: u8 = match self.cursor.shape {
                    CursorShape::Block => 2,
                    CursorShape::Underline => 4,
                    CursorShape::Bar => 6,
                };
                let _ = write!(self.replies, "{dcs_prefix}1$r{style} q{dcs_suffix}");
            }
            _ => {
                let _ = write!(self.replies, "{dcs_prefix}0$r{dcs_suffix}");
            }
        }
    }

    /// Render the cursor's current SGR attributes as a CSI parameter string
    /// (without the leading CSI / trailing `m`), for DECRQSS's `m` request.
    /// Always leads with `0` (reset) so replaying it reproduces the exact
    /// attribute set instead of merging onto whatever the requester's state
    /// already was.
    fn sgr_report_string(&self) -> String {
        let a = self.attrs;
        let mut parts = vec!["0".to_string()];
        if a.bold() {
            parts.push("1".to_string());
        }
        if a.dim() {
            parts.push("2".to_string());
        }
        if a.italic() {
            parts.push("3".to_string());
        }
        if a.underline() {
            parts.push("4".to_string());
        }
        if a.blink() {
            parts.push("5".to_string());
        }
        if a.inverse() {
            parts.push("7".to_string());
        }
        if a.invisible() {
            parts.push("8".to_string());
        }
        if a.strikethrough() {
            parts.push("9".to_string());
        }
        match a.fg {
            Color::Default => {}
            Color::Indexed(n) if n < 8 => parts.push((30 + n).to_string()),
            Color::Indexed(n) if n < 16 => parts.push((90 + n - 8).to_string()),
            Color::Indexed(n) => parts.push(format!("38;5;{n}")),
            Color::Rgb(r, g, b) => parts.push(format!("38;2;{r};{g};{b}")),
        }
        match a.bg {
            Color::Default => {}
            Color::Indexed(n) if n < 8 => parts.push((40 + n).to_string()),
            Color::Indexed(n) if n < 16 => parts.push((100 + n - 8).to_string()),
            Color::Indexed(n) => parts.push(format!("48;5;{n}")),
            Color::Rgb(r, g, b) => parts.push(format!("48;2;{r};{g};{b}")),
        }
        parts.join(";")
    }

    fn esc(&mut self, intermediate: Option<u8>, final_byte: u8) {
        // ESC sequences here (RIS, IND, RI, NEL, cursor save/restore) all move
        // or reset the cursor; none is a pure attribute change, so abandon any
        // deferred wrap.
        self.pending_wrap = false;
        match (intermediate, final_byte) {
            // RIS — reset to initial state
            (None, b'c') => {
                self.primary.clear();
                self.alternate.clear();
                self.cursor = Cursor::default();
                self.attrs = Attrs::default();
                self.alt_active = false;
                self.dirty.mark_all();
                self.mouse_mode = MouseMode::Off;
                self.sgr_mouse = false;
                self.keyboard_mode = KeyboardMode::Legacy;
                self.kitty_flag_stack.clear();
                self.app_cursor = false;
                self.app_keypad = false;
                self.autowrap = true;
                self.origin_mode = false;
                self.focus_events = false;
                let cols = self.active_screen().cols();
                self.tab_stops = Self::default_tab_stops(cols);
                self.g0_charset = Charset::Ascii;
                self.g1_charset = Charset::Ascii;
                self.shifted_to_g1 = false;
                self.link_table.clear();
                self.active_link_id = 0;
                self.published_links = 0;
                self.last_printed = None;
                self.reset_all_palette_overrides();
            }
            // SCS — designate G0 charset (`ESC ( X`). `0` = DEC Special
            // Graphics (line-drawing); anything else (`B` = US-ASCII, `A` =
            // UK, ...) we don't have distinct glyphs for, so it collapses to
            // plain ASCII passthrough.
            (Some(b'('), final_byte) => {
                self.g0_charset = if final_byte == b'0' {
                    Charset::DecSpecialGraphics
                } else {
                    Charset::Ascii
                };
            }
            // SCS — designate G1 charset (`ESC ) X`), same mapping as G0.
            (Some(b')'), final_byte) => {
                self.g1_charset = if final_byte == b'0' {
                    Charset::DecSpecialGraphics
                } else {
                    Charset::Ascii
                };
            }
            // DECPAM — application keypad mode.
            (None, b'=') => self.app_keypad = true,
            // DECPNM — normal keypad mode.
            (None, b'>') => self.app_keypad = false,
            // HTS — horizontal tab set: set a tab stop at the cursor column.
            (None, b'H') => self.set_tab_stop(),
            // DECSC — save cursor
            (None, b'7') => {
                self.saved_cursor = Some((self.cursor.col, self.cursor.row, self.attrs));
            }
            // DECRC — restore cursor
            (None, b'8') => {
                if let Some((col, row, attrs)) = self.saved_cursor {
                    self.cursor.col = col;
                    self.cursor.row = row;
                    self.attrs = attrs;
                }
            }
            // IND — index (move cursor down, scroll if at bottom)
            (None, b'D') => {
                self.cursor_down(1, true);
            }
            // RI — reverse index (move cursor up, scroll if at top)
            (None, b'M') => {
                let (top, bot) = self.active_screen().scroll_region();
                if self.cursor.row == top {
                    self.active_screen_mut().scroll_down(1, top, bot);
                    self.dirty.mark_rows(top, bot);
                } else {
                    self.cursor.row = self.cursor.row.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// Direct parser sink for the production parser-to-grid pipeline.
///
/// Each method mirrors the corresponding [`Grid::apply`] branch. Keeping the
/// state transition here avoids rebuilding and re-matching `TermEvent` for
/// every parser action, especially the high-volume printable ASCII runs.
impl TerminalSink for Grid {
    fn print(&mut self, ch: char) {
        self.snap_viewport_to_bottom();
        self.print(ch);
    }

    fn print_run(&mut self, bytes: &[u8]) {
        self.snap_viewport_to_bottom();
        self.print_ascii_run(bytes);
    }

    fn control(&mut self, control: C0) {
        self.snap_viewport_to_bottom();
        self.control(control);
    }

    fn esc(&mut self, intermediate: Option<u8>, final_byte: u8) {
        self.snap_viewport_to_bottom();
        self.esc(intermediate, final_byte);
    }

    fn csi(&mut self, action: CsiAction) {
        self.snap_viewport_to_bottom();
        self.csi(&action);
    }

    fn osc(&mut self, command: OscCmd<'_>) {
        self.snap_viewport_to_bottom();
        self.osc(&command);
    }

    fn dcs(&mut self, command: DcsCmd<'_>) {
        self.snap_viewport_to_bottom();
        self.dcs(&command);
    }

    fn ignored(&mut self) {
        self.snap_viewport_to_bottom();
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

impl core::fmt::Display for Grid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let screen = self.active_screen();
        for row in 0..screen.rows() {
            for col in 0..screen.cols() {
                let cell = screen.get_cell(col, row);
                write!(f, "{}", cell.ch)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vt_parser::Parser;

    fn apply_str(grid: &mut Grid, s: &[u8]) {
        let mut p = Parser::new();
        p.feed(s, |ev| grid.apply(&ev));
    }

    fn apply_direct(grid: &mut Grid, s: &[u8]) {
        let mut p = Parser::new();
        p.feed_to(s, grid);
    }

    #[test]
    fn direct_parser_sink_matches_event_pipeline() {
        // Exercise all dispatch families together, including a long ASCII run
        // and OSC/DCS payloads that borrow parser-owned buffers. The direct
        // pipeline must be observably identical to the compatibility event
        // pipeline, not merely faster on plain output.
        let input = b"hello terminal output\x1b[31m red\x1b[0m\r\n\x1b]2;title\x07\x1bP$qm\x1b\\\x1b[2;4Hdone";
        let mut events = Grid::new(20, 4);
        let mut direct = Grid::new(20, 4);
        apply_str(&mut events, input);
        apply_direct(&mut direct, input);
        assert_eq!(events.to_string(), direct.to_string());
        assert_eq!(events.cursor(), direct.cursor());
        let events_dirty = events.take_dirty();
        let direct_dirty = direct.take_dirty();
        assert_eq!(events_dirty.rows.len(), direct_dirty.rows.len());
        assert_eq!(events_dirty.cols, direct_dirty.cols);
        assert_eq!(events_dirty.cursor, direct_dirty.cursor);
        assert_eq!(events_dirty.cursor_changed, direct_dirty.cursor_changed);
        assert_eq!(events_dirty.content_delta, direct_dirty.content_delta);
        assert_eq!(
            format!("{:?}", events_dirty.kind),
            format!("{:?}", direct_dirty.kind)
        );
        for (event_row, direct_row) in events_dirty.rows.iter().zip(&direct_dirty.rows) {
            assert_eq!(event_row.row, direct_row.row);
            assert_eq!(
                event_row.content.materialize_cells(),
                direct_row.content.materialize_cells()
            );
        }
        assert_eq!(events.take_title(), direct.take_title());
        assert_eq!(events.take_replies(), direct.take_replies());
    }

    const CJK_ZHONG: &str = "\u{4e2d}"; // '中' (Chinese for "middle") — 2 cells wide

    #[test]
    fn bracketed_paste_mode_tracked() {
        let mut g = Grid::new(10, 3);
        assert!(!g.bracketed_paste());
        apply_str(&mut g, b"\x1b[?2004h"); // DECSET 2004 — enable
        assert!(g.bracketed_paste());
        apply_str(&mut g, b"\x1b[?2004l"); // DECRST 2004 — disable
        assert!(!g.bracketed_paste());
    }

    #[test]
    fn scroll_batched_with_inplace_edit_yields_precise_rows_and_scroll_span() {
        // The flood/arrow-key bug: a snapshot that batches a content scroll
        // together with an in-place edit of an already-visible row must report
        // (a) a scroll_span for the pixel-shift and (b) *only* the genuinely
        // changed rows -- not the whole region -- so the renderer shifts then
        // paints exactly those rows. Previously the region was blanket-marked,
        // so the edit was indistinguishable from a shifted row and got lost.
        let mut g = Grid::new(10, 4);
        apply_str(&mut g, b"l0\r\nl1\r\nl2\r\nl3"); // fill 4 rows, cursor on row 3
        let _ = g.take_dirty(); // drain

        // One newline scrolls everything up by 1 (l0 into scrollback), then we
        // overwrite the top visible row in place -- all in one batch, one
        // snapshot.
        apply_str(&mut g, b"\r\n"); // scroll up by 1; cursor now row 3, col 0
        apply_str(&mut g, b"\x1b[1;1H"); // cursor home (row 0)
        apply_str(&mut g, b"X"); // in-place edit of row 0
        let snap = g.take_dirty();

        // The whole screen shifted up by 1.
        assert_eq!(
            snap.scroll_span(),
            Some(ScrollBand {
                top: 0,
                bot: 3,
                delta: 1
            })
        );
        // content_delta tracks the scrollback push (1 line).
        assert_eq!(snap.content_delta, 1);
        // Dirty rows: row 0 (the 'X' edit) and row 3 (the freshly-exposed
        // blank bottom the scroll pulled in) -- NOT all four rows.
        let mut rows: Vec<u16> = snap.rows.iter().map(|r| r.row).collect();
        rows.sort_unstable();
        assert_eq!(rows, vec![0, 3]);
        // The edit really is 'X' at row 0 col 0.
        assert_eq!(
            snap.rows
                .iter()
                .find(|r| r.row == 0)
                .unwrap()
                .content
                .materialize_cells()[0]
                .ch,
            'X'
        );
    }

    #[test]
    fn take_dirty_reports_content_scroll_delta() {
        // Fill a 3-row screen, then push 2 more lines so 2 scroll into
        // scrollback. The next dirty snapshot must report content_delta == 2
        // so the renderer can shift its selection to follow the text.
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"l0\r\nl1\r\nl2\r\n");
        let _ = g.take_dirty(); // drain the fill
        apply_str(&mut g, b"l3\r\nl4\r\n"); // 2 more lines -> 2 scroll off top
        let snap = g.take_dirty();
        assert_eq!(snap.content_delta, 2);
        // Delta resets after being reported.
        let snap2 = g.take_dirty();
        assert_eq!(snap2.content_delta, 0);
    }

    #[test]
    fn requeue_snapshot_re_emits_lost_rows_on_next_take_dirty() {
        // Simulate a dirty_tx.push failure: take a snapshot, then requeue it
        // (as if the ring were full) instead of delivering it. The next
        // take_dirty must reproduce the same rows -- nothing gets lost.
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"hello");
        let snap = g.take_dirty();
        assert!(!snap.rows.is_empty());
        let row_indices: Vec<u16> = snap.rows.iter().map(|r| r.row).collect();

        g.requeue_snapshot(&snap);

        let snap2 = g.take_dirty();
        let row_indices2: Vec<u16> = snap2.rows.iter().map(|r| r.row).collect();
        assert_eq!(row_indices, row_indices2);
    }

    #[test]
    fn requeue_snapshot_restores_cursor_changed_and_content_delta() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"l0\r\nl1\r\nl2\r\n");
        let _ = g.take_dirty(); // drain fill, establishes last_cursor
        apply_str(&mut g, b"l3\r\nl4"); // scrolls some lines; moves the cursor
        let snap = g.take_dirty();
        assert!(snap.cursor_changed);
        assert!(snap.content_delta > 0);
        let delta = snap.content_delta;

        // Requeue as if delivery failed -- nothing else touches the grid in
        // between, matching the real pty/parser thread's usage.
        g.requeue_snapshot(&snap);

        // A no-op take_dirty (no new apply calls) would normally report
        // cursor_changed = false (the cursor hasn't moved further) and
        // content_delta = 0 -- but the requeue must force both back on so
        // the renderer still sees what it missed.
        let snap2 = g.take_dirty();
        assert!(snap2.cursor_changed);
        assert_eq!(snap2.content_delta, delta);

        // And it's a one-shot: the next take_dirty is back to normal.
        let snap3 = g.take_dirty();
        assert!(!snap3.cursor_changed);
        assert_eq!(snap3.content_delta, 0);
    }

    #[test]
    fn print_text() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"hi");
        assert_eq!(g.screen().get_cell(0, 0).ch, 'h');
        assert_eq!(g.screen().get_cell(1, 0).ch, 'i');
        assert_eq!(g.cursor(), (2, 0));
    }

    #[test]
    fn cursor_movement() {
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"\x1b[3;5H"); // CUP row=3, col=5
        assert_eq!(g.cursor(), (4, 2)); // 0-indexed
    }

    #[test]
    fn erase_line() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"hello\x1b[1G\x1b[2K"); // print then erase whole line
        for col in 0..5u16 {
            assert_eq!(g.screen().get_cell(col, 0).ch, ' ');
        }
    }

    #[test]
    fn sgr_bold() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"\x1b[1mA");
        assert!(g.screen().get_cell(0, 0).attrs.bold());
    }

    #[test]
    fn carriage_return_line_feed() {
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"AB\r\nCD");
        assert_eq!(g.screen().get_cell(0, 0).ch, 'A');
        assert_eq!(g.screen().get_cell(1, 0).ch, 'B');
        assert_eq!(g.screen().get_cell(0, 1).ch, 'C');
        assert_eq!(g.screen().get_cell(1, 1).ch, 'D');
    }

    #[test]
    fn resize_to_zero_does_not_panic() {
        // A compositor can send a 0-cell configure when dragged to nothing.
        // Clamp to 1x1 instead of overflowing on `cols - 1`.
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"hello\r\nworld");
        g.resize(0, 0);
        assert_eq!(g.screen().cols(), 1);
        assert_eq!(g.screen().rows(), 1);
        // A subsequent erase (which does `cols - 1`) must not overflow.
        apply_str(&mut g, b"\x1b[2K");
        // And growing back works.
        g.resize(10, 5);
        assert_eq!(g.screen().cols(), 10);
        assert_eq!(g.screen().rows(), 5);
    }

    #[test]
    fn resize_shrink_keeps_bottom_content() {
        // 5 rows, cursor ends on the last row (typical: prompt at bottom).
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"L0\r\nL1\r\nL2\r\nL3\r\nL4");
        assert_eq!(g.cursor(), (2, 4));
        // Shrink to 3 rows: should keep L2,L3,L4 (the bottom), not L0,L1,L2.
        g.resize(10, 3);
        assert_eq!(g.screen().get_cell(0, 0).ch, 'L');
        assert_eq!(g.screen().get_cell(1, 0).ch, '2');
        assert_eq!(g.screen().get_cell(1, 2).ch, '4');
        assert_eq!(g.cursor(), (2, 2));
    }

    #[test]
    fn resize_shrink_keeps_prompt_when_cursor_low() {
        // Single prompt line at the top, cursor right after it (row 0). A naive
        // used=cursor.row+1 would keep only blank bottom rows and erase this.
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"$ ");
        assert_eq!(g.cursor(), (2, 0));
        g.resize(10, 2);
        assert_eq!(g.screen().get_cell(0, 0).ch, '$');
        assert_eq!(g.cursor(), (2, 0));
    }

    #[test]
    fn full_line_then_newline_is_not_wrapped() {
        // A line that exactly fills the width followed by a hard newline must
        // NOT be marked wrapped — otherwise resize stitches the next line onto
        // it (the "merges into the top one" bug). Deferred wrap means the
        // newline arrives before any glyph realizes the wrap.
        let mut g = Grid::new(10, 4);
        apply_str(&mut g, b"0123456789\r\nNEXT");
        assert!(
            !g.screen().is_wrapped(0),
            "full line + newline must not wrap"
        );
        // After resize the two lines must stay separate: NEXT begins at col 0
        // of its own row, not appended after row 0.
        g.resize(10, 4);
        assert_eq!(g.screen().get_cell(0, 1).ch, 'N');
        assert_eq!(g.screen().get_cell(0, 0).ch, '0');
    }

    #[test]
    fn snapshot_reports_a_wrap_realized_after_the_row_was_flushed() {
        let mut g = Grid::new(10, 4);
        apply_str(&mut g, b"0123456789");
        let first = g.take_dirty();
        assert_eq!(first.rows[0].soft_wrap, None);
        apply_str(&mut g, b"X");
        let second = g.take_dirty();
        let row0 = second
            .rows
            .iter()
            .find(|r| r.row == 0)
            .expect("realizing the wrap re-dirties the wrapped row");
        assert_eq!(row0.soft_wrap, Some(10));
    }

    #[test]
    fn full_line_then_glyph_is_wrapped() {
        // A line that fills the width and is immediately followed by another
        // glyph genuinely wraps: the deferred wrap is realized on that glyph.
        let mut g = Grid::new(10, 4);
        apply_str(&mut g, b"0123456789X");
        assert!(g.screen().is_wrapped(0), "full line + glyph must wrap");
        assert_eq!(g.screen().get_cell(0, 1).ch, 'X');
        assert_eq!(g.cursor(), (1, 1));
    }

    #[test]
    fn a_right_aligned_prompt_is_never_clipped_off_the_right_edge() {
        // User-reported: with fish's RPROMPT (a timestamp padded out to the
        // right margin) a HORIZONTAL shrink left the newest line's timestamp
        // running off the right edge, cut mid-text -- and worse the smaller
        // the window got. Pressing Enter did not clear it, which rules out
        // the shell reprinting at a stale width: a freshly drawn prompt
        // landed wrong too, so the fault is in our own reflow.
        //
        // The invariant this pins down is narrow but absolute: whatever
        // resize does to a line's *content*, it may never leave a cell
        // sitting at a column >= the new width. Every physical row must fit
        // the grid it was reflowed into.
        let mut g = Grid::new(80, 24);
        // A prompt line shaped like the real one: short left prompt, a run of
        // padding, then a right-aligned timestamp ending exactly at col 80.
        let stamp = "09:50:12";
        let left = "~ > ";
        let pad = 80 - left.len() - stamp.len();
        apply_str(
            &mut g,
            format!("{left}{}{stamp}", " ".repeat(pad)).as_bytes(),
        );

        // Shrink horizontally, progressively -- the reported trigger.
        for w in [72u16, 64, 56, 48, 40, 32, 24] {
            g.resize(w, 24);
            let s = g.screen();
            assert_eq!(s.cols(), w, "screen width must track the resize at w={w}");
            // Nothing may survive past the last legal column.
            for r in 0..s.rows() {
                for c in 0..s.cols() {
                    let _ = s.get_cell(c, r);
                }
            }
            // The timestamp's digits must all still be reachable inside the
            // grid: count them across the whole visible area. Losing some to
            // an off-grid column is exactly the reported clipping.
            let digits: usize = (0..s.rows())
                .flat_map(|r| (0..s.cols()).map(move |c| (c, r)))
                .filter(|&(c, r)| s.get_cell(c, r).ch.is_ascii_digit())
                .count();
            assert_eq!(
                digits, 6,
                "at width {w} the timestamp lost digits -- clipped off the edge"
            );
        }
    }

    #[test]
    fn a_run_of_interior_padding_survives_a_narrow_then_wide_drag() {
        // User-reported: fish RPROMPT timestamps COLLAPSE leftward after a
        // horizontal resize -- "~ > 09:49:39" jammed against the prompt
        // instead of right-aligned. Not clipping (the earlier misdiagnosis):
        // the run of interior padding that positions the timestamp is being
        // squashed.
        //
        // Mechanism: shrinking word-wraps the line inside its padding run, so
        // the padding becomes TRAILING blanks on a continuation row. The
        // stitch step in `Screen::resize` trims a ragged continuation row's
        // trailing blanks and restores exactly ONE separator space -- correct
        // for a word break, but it cannot tell a one-space word separator from
        // an 80-space alignment run, because both are `Cell::default()`. So a
        // whole padding run collapses to a single space, permanently. That
        // matches every reported symptom: horizontal-only, worse the smaller
        // the window (more breaks inside the run), and unaffected by Enter
        // (the damage is already in the scrollback, not in what fish prints).
        let row_text = |g: &Grid, r: u16| -> String {
            let s = g.screen();
            (0..s.cols()).map(|c| s.get_cell(c, r).ch).collect()
        };
        let mut g = Grid::new(100, 24);
        let stamp = "10:02:38";
        let left = "~ > ";
        let pad = 100 - left.len() - stamp.len();
        apply_str(
            &mut g,
            format!("{left}{}{stamp}", " ".repeat(pad)).as_bytes(),
        );
        let before = row_text(&g, 0);

        // Shrink (wrapping inside the padding run) and come back.
        for w in [60u16, 100] {
            g.resize(w, 24);
        }

        let after = row_text(&g, 0);
        assert_eq!(
            after.trim_end(),
            before.trim_end(),
            "padding run collapsed: the right-aligned timestamp moved left"
        );

        // A real drag is many steps, and the collapse used to compound: each
        // narrow<->wide cycle ate more of the run. Walk down and back up.
        for w in [90u16, 80, 70, 60, 50, 40, 50, 60, 70, 80, 90, 100] {
            g.resize(w, 24);
        }
        assert_eq!(
            row_text(&g, 0).trim_end(),
            before.trim_end(),
            "padding eroded across a multi-step drag"
        );

        // NOTE: at an intermediate width the stamp is deliberately NOT pinned
        // to the right margin. A 100-column logical line reflowed to 60 wraps
        // onto a second row, so the stamp sits at column 40 of that row -- the
        // padding is preserved, it just no longer reaches a margin that moved.
        // Every terminal that reflows behaves this way; only the shell can
        // re-right-align, on its next prompt. What matters is that the run is
        // not DESTROYED, which the round-trips above prove.
        g.resize(60, 24);
        let s = g.screen();
        let stamp_cells: usize = (0..s.rows())
            .flat_map(|r| (0..s.cols()).map(move |c| (c, r)))
            .filter(|&(c, r)| s.get_cell(c, r).ch.is_ascii_digit())
            .count();
        assert_eq!(
            stamp_cells, 6,
            "the stamp's digits must all survive at 60 cols"
        );
    }

    #[test]
    fn resize_drag_roundtrip_does_not_grow_lines_or_drop_content() {
        // Regression: word-aware reflow leaves ragged (blank-padded) soft-wrap
        // rows. The resize stitch step must trim that padding and restore only
        // ONE separator space per word break; otherwise every narrow<->wide
        // cycle injected an extra space, growing each line until it spilled a
        // physical row into scrollback -- so a drag silently deleted visible
        // lines from the top. Dragging the width down and back must be a
        // perfect round-trip of the visible text.
        let visible = |g: &Grid| -> Vec<String> {
            let s = g.screen();
            (0..s.rows())
                .map(|r| {
                    (0..s.cols())
                        .map(|c| s.get_cell(c, r).ch)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };
        let mut g = Grid::new(80, 24);
        for i in 0..20 {
            apply_str(
                &mut g,
                format!("line{i:02} some words here to fill the row nicely\r\n").as_bytes(),
            );
        }
        let before = visible(&g);
        // A realistic horizontal drag: many width steps down, then back to 80.
        for w in [72u16, 64, 56, 48, 40, 48, 56, 64, 72, 80] {
            g.resize(w, 24);
        }
        assert_eq!(before, visible(&g), "drag roundtrip lost or mutated lines");
        // No line grew a phantom space at the old wrap column.
        assert!(
            visible(&g).iter().all(|l| !l.contains("  ")),
            "a reflow injected a double space"
        );
    }

    #[test]
    fn resize_drag_many_short_prompt_lines_no_phantoms() {
        // Reproduce the user's real corruption: a scrollback full of SHORT
        // hard-newline-separated prompt lines (each fits a wide width but
        // wraps at a narrow one), dragged down through many widths and back.
        // The reported artifact was truncated fragments (e.g. `~/C/f/term`,
        // 10 cols) and several prompts concatenated onto one row -- i.e. the
        // stitch/reflow mangling short lines across repeated narrow<->wide
        // steps, not the single long-line growth the other roundtrip covers.
        let visible = |g: &Grid| -> Vec<String> {
            let s = g.screen();
            (0..s.rows())
                .map(|r| {
                    (0..s.cols())
                        .map(|c| s.get_cell(c, r).ch)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };
        let mut g = Grid::new(80, 24);
        // 30 short prompt lines, each its own hard-newline logical line.
        for _ in 0..30 {
            apply_str(&mut g, b"~/C/f/terminal > \r\n");
        }
        let before = visible(&g);
        // A thrashing horizontal drag: many steps down (narrow enough to wrap
        // the 16-char prompt) and back up to 80.
        for w in [40u16, 10, 30, 8, 24, 12, 50, 16, 64, 10, 72, 80] {
            g.resize(w, 24);
        }
        assert_eq!(
            before,
            visible(&g),
            "short-line drag roundtrip corrupted the scrollback"
        );
        // No row holds two prompts stitched together, and none is a truncated
        // fragment left from a narrow width.
        for line in visible(&g) {
            assert!(
                line.matches('>').count() <= 1,
                "two prompts concatenated on one row: {line:?}"
            );
        }
    }

    #[test]
    fn resize_interleaved_with_shell_reprompt_no_fragments() {
        // The faithful repro of the user's corruption: during a fast drag the
        // shell gets a SIGWINCH at EVERY intermediate width and redraws its
        // prompt (zsh `reset-prompt`: CR, erase-to-end-of-display, reprint).
        // Our reflow and the shell's reprint interleave, so a reprint made at
        // width 10 can be reflowed by the next step before the shell clears
        // it -- stranding a truncated fragment. After the drag settles at a
        // wide width and the shell does its FINAL redraw, the screen must hold
        // exactly one clean prompt with no leftover fragments above it.
        let visible = |g: &Grid| -> Vec<String> {
            let s = g.screen();
            (0..s.rows())
                .map(|r| {
                    (0..s.cols())
                        .map(|c| s.get_cell(c, r).ch)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect()
        };
        // zsh reset-prompt: return to the prompt's first row, clear from there
        // to end of screen, reprint. Modelled as CR + ED0 + prompt text. (The
        // cursor is on the prompt's own line; for a single-row-at-wide prompt
        // the up-count is 0, but at narrow widths the prompt wrapped, so the
        // shell moves up by however many rows IT thinks the prompt spans.)
        let reprompt = |g: &mut Grid, width: u16| {
            // The shell computes its own wrap for a 17-cell prompt over
            // `width`: rows_used = ceil(len/width), cursor on the last row, so
            // it moves up rows_used-1 to reach the prompt's first row.
            let len = 17u16;
            let rows_used = len.div_ceil(width.max(1));
            let up = rows_used.saturating_sub(1);
            let mut seq = Vec::new();
            seq.push(b'\r');
            if up > 0 {
                seq.extend_from_slice(format!("\x1b[{up}A").as_bytes());
            }
            seq.extend_from_slice(b"\x1b[J~/C/f/terminal > ");
            apply_str(g, &seq);
        };
        let mut g = Grid::new(80, 24);
        // Some committed history so the prompt sits mid-screen.
        for i in 0..5 {
            apply_str(&mut g, format!("output line {i}\r\n").as_bytes());
        }
        apply_str(&mut g, b"~/C/f/terminal > ");
        // Thrash: resize then shell-reprompt. A real shell remembers how many
        // rows it drew at the PREVIOUS width and moves up by THAT count (its
        // own bookkeeping), not the current one -- the terminal has already
        // reflowed underneath it. Feed the previous width to `reprompt` to
        // model that lag.
        let mut prev_w = 80u16;
        for w in [40u16, 10, 30, 8, 24, 12, 50, 16, 64, 10, 72, 80] {
            g.resize(w, 24);
            reprompt(&mut g, prev_w);
            prev_w = w;
        }
        // Final settle: the shell's last redraw is consistent with the
        // settled width (SIGWINCH coalesced to the final size).
        reprompt(&mut g, 80);
        let vis = visible(&g);
        // Exactly one prompt, no fragments: count rows containing '>'.
        let prompt_rows = vis.iter().filter(|l| l.contains('>')).count();
        assert_eq!(
            prompt_rows, 1,
            "expected one clean prompt, got fragments: {vis:?}"
        );
        // And the committed output above must be intact and un-fragmented.
        let joined: String = vis.join("\n");
        assert!(
            joined.contains("output line 0"),
            "history corrupted: {vis:?}"
        );
    }

    #[test]
    fn resize_narrow_reflows_wrapped_line() {
        // 10 cols: print 15 chars -> auto-wraps to 2 rows (10 + 5).
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"abcdefghijklmno");
        assert_eq!(g.screen().get_cell(0, 0).ch, 'a');
        assert_eq!(g.screen().get_cell(0, 1).ch, 'k');
        // Narrow to 5 cols: the same logical line re-breaks into 3 rows.
        g.resize(5, 5);
        assert_eq!(g.screen().get_cell(0, 0).ch, 'a');
        assert_eq!(g.screen().get_cell(4, 0).ch, 'e');
        assert_eq!(g.screen().get_cell(0, 1).ch, 'f');
        assert_eq!(g.screen().get_cell(0, 2).ch, 'k');
        assert_eq!(g.screen().get_cell(4, 2).ch, 'o');
    }

    #[test]
    fn resize_widen_reflows_back() {
        // Narrow grid, wrapped line, then widen: characters rejoin one row.
        let mut g = Grid::new(5, 5);
        apply_str(&mut g, b"abcdefghij"); // 2 rows of 5
        g.resize(10, 5);
        for (i, ch) in (b'a'..=b'j').enumerate() {
            let col = u16::try_from(i).expect("test column fits in u16");
            assert_eq!(g.screen().get_cell(col, 0).ch, ch as char);
        }
    }

    #[test]
    fn resize_hard_newline_not_joined() {
        // Two separate logical lines (hard newline) must NOT be stitched.
        let mut g = Grid::new(10, 5);
        apply_str(&mut g, b"abc\r\ndef");
        g.resize(20, 5);
        assert_eq!(g.screen().get_cell(0, 0).ch, 'a');
        assert_eq!(g.screen().get_cell(0, 1).ch, 'd'); // stays on its own row
    }

    #[test]
    fn resize_cursor_at_wrap_boundary() {
        // Exactly 10 chars at width 10 with cursor pending-wrap at col 10
        // (i.e. just printed the 10th char, autowrap armed). Narrow to 5: the
        // logical line is 10 cells -> 2 full rows; the cursor sits at the START
        // of a notional 3rd row. It must not be clamped onto the end of row 1.
        let mut g = Grid::new(10, 4);
        apply_str(&mut g, b"0123456789");
        // Deferred wrap: after printing the 10th char the cursor visually rests
        // on the last column (9, 0) with the wrap armed but not yet realized.
        assert_eq!(g.cursor(), (9, 0));
        g.resize(5, 4);
        // 10 cells over width 5 = rows of "01234" / "56789"; cursor was at
        // logical offset 10 -> start of the next row.
        assert_eq!(g.screen().get_cell(0, 0).ch, '0');
        assert_eq!(g.screen().get_cell(0, 1).ch, '5');
        assert_eq!(g.cursor(), (0, 2));
    }

    #[test]
    fn resize_prompt_to_very_small_width() {
        // A prompt longer than the target width must reflow without dropping or
        // duplicating cells, cursor following to the tail.
        let mut g = Grid::new(20, 4);
        apply_str(&mut g, b"user@host:~/dir$ ");
        let (cx, _) = g.cursor();
        assert_eq!(cx, 17);
        g.resize(4, 6);
        // 17 chars over width 4: rows "user"|"@hos"|"t:~/"|"dir$"|" " — the
        // trailing space is char 16, cursor parked just past it. Verify the
        // reflow split and that the cursor follows to the tail.
        assert_eq!(g.screen().get_cell(0, 0).ch, 'u');
        assert_eq!(g.screen().get_cell(0, 1).ch, '@');
        assert_eq!(g.screen().get_cell(0, 3).ch, 'd');
        assert_eq!(g.cursor(), (1, 4));
    }

    #[test]
    fn resize_then_shell_redraw_leaves_no_phantom_prompt_rows() {
        // Reproduce the "phantom copies of the prompt on the line above" bug.
        // A shell (zsh/fish) redraws its prompt on SIGWINCH by moving the
        // cursor to the prompt's start column/row and reprinting. If our
        // reflow parks the cursor on a DIFFERENT row than where the shell
        // believes its prompt begins, the reprint lands offset and the old
        // wrapped fragment above is never overwritten -> a phantom row.
        //
        // Model: a 24-col prompt that wraps once at width 20, then the window
        // is dragged to width 10 (prompt now spans 3 rows). The shell reacts
        // by returning to column 0 of the CURRENT cursor row via CR and
        // reprinting the whole prompt from there. After the redraw the visible
        // area must contain exactly ONE copy of the prompt tail, with no
        // orphaned fragment on an earlier row.
        let mut g = Grid::new(20, 6);
        // A 25-column prompt so it soft-wraps at the old width of 20.
        let prompt = b"~/Code/forfun/terminal > ";
        assert_eq!(prompt.len(), 25);
        apply_str(&mut g, prompt);
        // Drag narrower: reflow to width 10. A shell's own layout model
        // character-wraps, so it believes the 25-col prompt now occupies
        // ceil(25/10)=3 rows with the cursor on row index 2. Our reflow must
        // agree, or the shell's redraw lands offset.
        g.resize(10, 6);
        let (_, cy) = g.cursor();
        assert_eq!(
            cy, 2,
            "reflowed cursor row must match the shell's char-wrap model"
        );
        // The shell's SIGWINCH handler: move the cursor UP to the prompt's
        // first row (by its own row count), carriage-return to column 0,
        // clear from there to the end of screen (CSI J), then reprint. This
        // only erases the whole prompt if our cursor sat where the shell
        // thinks it did.
        apply_str(&mut g, b"\x1b[2A\r\x1b[J~/Code/forfun/terminal > ");
        // Concatenate every visible row (no separators) and count how many
        // times the prompt appears. A phantom leaves a second, offset copy;
        // a clean redraw leaves exactly one. Wrap boundaries fall inside the
        // string, so join with no separator before matching.
        let text = format!("{g}");
        let concatenated: String = text.lines().map(str::trim_end).collect();
        let occurrences = concatenated.matches("~/Code/forfun/terminal >").count();
        assert_eq!(
            occurrences, 1,
            "prompt duplicated after resize+redraw: {concatenated:?}"
        );
    }

    #[test]
    fn scrolling_keeps_lines_contiguous_no_blank_gap() {
        // Regression test for the "blank line appears between every line after
        // the screen first fills" bug: scroll_up used to drop a visible row
        // each scroll, leaving a gap. Fill a small screen, then keep feeding
        // lines; every content row must sit immediately below the previous
        // with NO blank row in between.
        let mut g = Grid::new(20, 4);
        for i in 0..10 {
            let line = format!("line{i}\r\n");
            apply_str(&mut g, line.as_bytes());
        }
        // The last 3 lines are visible, contiguous, on rows 0..=2; row 3 is the
        // fresh cursor line (legitimately blank).
        let text = format!("{g}");
        let vis: Vec<&str> = text.lines().collect();
        assert_eq!(vis[0].trim_end(), "line7");
        assert_eq!(vis[1].trim_end(), "line8");
        assert_eq!(vis[2].trim_end(), "line9");
        assert_eq!(vis[3].trim_end(), "");
    }

    #[test]
    fn viewport_scrolls_into_scrollback_and_snaps_back_on_output() {
        // Fill a small screen and overflow it so scrollback accumulates.
        let mut g = Grid::new(20, 4);
        for i in 0..10 {
            apply_str(&mut g, format!("line{i}\r\n").as_bytes());
        }
        assert!(g.primary.scrollback_len() > 0);
        assert_eq!(g.viewport_offset(), 0);

        // Scroll up 3 lines: the top visible row should now be older history.
        g.scroll_viewport(3);
        assert_eq!(g.viewport_offset(), 3);
        let snap = g.take_dirty();
        // A viewport move repaints the whole frame.
        assert_eq!(snap.rows.len(), 4);
        // row 0 of a 3-up view = scrollback line (len-3). Verify it differs
        // from the live bottom row 0 by checking it's a real history line.
        let row0: String = snap.rows[0]
            .content
            .materialize_cells()
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            row0.starts_with("line"),
            "expected a history line, got {row0:?}"
        );

        // Scrolling further clamps at the top of history.
        g.scroll_viewport(10_000);
        let max = g.viewport_offset();
        assert_eq!(max, g.primary.scrollback_len());

        // New output snaps back to the live bottom.
        apply_str(&mut g, b"X");
        assert_eq!(g.viewport_offset(), 0);
        let snap = g.take_dirty();
        assert_eq!(snap.rows.len(), 4); // snap-back also repaints fully
        let row3: String = snap.rows[3]
            .content
            .materialize_cells()
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            row3.starts_with('X'),
            "cursor line back at bottom: {row3:?}"
        );
    }

    #[test]
    fn viewport_scroll_down_clamps_at_zero() {
        let mut g = Grid::new(20, 4);
        for i in 0..10 {
            apply_str(&mut g, format!("line{i}\r\n").as_bytes());
        }
        g.scroll_viewport(2);
        assert_eq!(g.viewport_offset(), 2);
        g.scroll_viewport(-5); // overshoot the bottom
        assert_eq!(g.viewport_offset(), 0);
    }

    #[test]
    fn viewport_snapshot_emits_signed_pixel_shift_for_small_steps() {
        let mut g = Grid::new(20, 4);
        for i in 0..20 {
            apply_str(&mut g, format!("line{i}\r\n").as_bytes());
        }
        // Drain the output frame (as the render loop does every vsync) so the
        // accumulated content_delta resets and rendered_offset is 0 -- the
        // baseline a pure wheel-scroll shifts away from.
        let _ = g.take_dirty();
        // First scroll from the live bottom (rendered_offset = 0): a small step
        // up into history reveals rows at the top => shift = 0 - 2 = -2.
        g.scroll_viewport(2);
        let snap = g.take_dirty();
        assert_eq!(snap.viewport_offset(), Some(2));
        assert_eq!(snap.viewport_shift(), Some(-2));
        assert_eq!(snap.rows.len(), 4, "full row set is still carried");

        // Scroll one more line up: 2 -> 3, shift = 2 - 3 = -1.
        g.scroll_viewport(1);
        let snap = g.take_dirty();
        assert_eq!(snap.viewport_shift(), Some(-1));

        // Scroll back down toward the bottom: 3 -> 1, reveal at bottom,
        // shift = 3 - 1 = +2.
        g.scroll_viewport(-2);
        let snap = g.take_dirty();
        assert_eq!(snap.viewport_offset(), Some(1));
        assert_eq!(snap.viewport_shift(), Some(2));
    }

    #[test]
    fn viewport_snapshot_full_repaint_when_step_exceeds_screen() {
        let mut g = Grid::new(20, 4);
        for i in 0..40 {
            apply_str(&mut g, format!("line{i}\r\n").as_bytes());
        }
        let _ = g.take_dirty();
        // A jump larger than the screen height (4) leaves nothing to reuse, so
        // no shift is emitted -- a full repaint.
        g.scroll_viewport(10);
        let snap = g.take_dirty();
        assert_eq!(snap.viewport_offset(), Some(10));
        assert_eq!(snap.viewport_shift(), None);
    }

    #[test]
    fn requeued_viewport_snapshot_distrusts_rendered_frame_and_repaints_full() {
        let mut g = Grid::new(20, 4);
        for i in 0..20 {
            apply_str(&mut g, format!("line{i}\r\n").as_bytes());
        }
        let _ = g.take_dirty();
        g.scroll_viewport(2);
        let snap = g.take_dirty();
        assert_eq!(snap.viewport_shift(), Some(-2));
        // Pretend the ring rejected it: requeue must distrust the (never
        // delivered) frame, so the re-armed snapshot repaints fully.
        g.requeue_snapshot(&snap);
        let snap2 = g.take_dirty();
        assert_eq!(snap2.viewport_offset(), Some(2));
        assert_eq!(
            snap2.viewport_shift(),
            None,
            "must not pixel-shift from a frame that never landed"
        );
        assert_eq!(snap2.rows.len(), 4);
    }

    #[test]
    fn erase_display_ed2_and_ed3_wipe_scrollback() {
        // Fill + overflow so scrollback exists, then ED 2 (what `clear` sends)
        // must drop it: no scrolling back afterwards.
        for seq in [&b"\x1b[2J"[..], &b"\x1b[3J"[..]] {
            let mut g = Grid::new(20, 4);
            for i in 0..10 {
                apply_str(&mut g, format!("line{i}\r\n").as_bytes());
            }
            assert!(g.primary.scrollback_len() > 0, "setup: scrollback exists");
            apply_str(&mut g, seq);
            assert_eq!(g.primary.scrollback_len(), 0, "scrollback wiped by {seq:?}");
            // Viewport can't scroll into history anymore.
            g.scroll_viewport(10);
            assert_eq!(g.viewport_offset(), 0);
        }
    }

    #[test]
    fn dirty_snapshot() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"X");
        let snap = g.take_dirty();
        assert!(!snap.is_empty());
        // After taking, should be clean.
        let snap2 = g.take_dirty();
        assert!(snap2.is_empty());
    }

    // ── Wide-char (CJK/emoji) tests ──────────────────────────────────

    #[test]
    fn wide_char_occupies_two_cells() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, CJK_ZHONG.as_bytes());
        let lead = g.screen().get_cell(0, 0);
        let spacer = g.screen().get_cell(1, 0);
        assert_eq!(lead.ch, CJK_ZHONG.chars().next().unwrap());
        assert!(lead.wide());
        assert!(!lead.spacer());
        assert!(spacer.spacer());
        assert!(!spacer.wide());
        assert_eq!(g.cursor(), (2, 0));
    }

    #[test]
    fn mixed_ascii_and_wide_advance_correctly() {
        let mut g = Grid::new(10, 3);
        let s = format!("a{CJK_ZHONG}b");
        apply_str(&mut g, s.as_bytes());
        assert_eq!(g.screen().get_cell(0, 0).ch, 'a');
        assert_eq!(
            g.screen().get_cell(1, 0).ch,
            CJK_ZHONG.chars().next().unwrap()
        );
        assert!(g.screen().get_cell(2, 0).spacer());
        assert_eq!(g.screen().get_cell(3, 0).ch, 'b');
        assert_eq!(g.cursor(), (4, 0));
    }

    #[test]
    fn wide_char_wraps_whole_when_it_would_straddle_margin() {
        // 5-col grid: "abcd" fills 4 cols, one column left -- a wide char
        // must not split across the margin; it wraps whole onto the next row.
        let mut g = Grid::new(5, 3);
        let s = format!("abcd{CJK_ZHONG}");
        apply_str(&mut g, s.as_bytes());
        assert_eq!(g.screen().get_cell(4, 0).ch, ' ');
        assert!(g.screen().is_wrapped(0));
        assert_eq!(
            g.screen().get_cell(0, 1).ch,
            CJK_ZHONG.chars().next().unwrap()
        );
        assert!(g.screen().get_cell(1, 1).spacer());
        assert_eq!(g.cursor(), (2, 1));
    }

    #[test]
    fn zero_width_combining_mark_does_not_advance_cursor() {
        let mut g = Grid::new(10, 3);
        let s = "e\u{0301}"; // e + combining acute
        apply_str(&mut g, s.as_bytes());
        assert_eq!(g.cursor(), (1, 0));
    }

    #[test]
    fn erase_line_clears_orphaned_wide_pair() {
        let mut g = Grid::new(10, 3);
        let s = format!("{CJK_ZHONG}x");
        apply_str(&mut g, s.as_bytes());
        // Move to col 2 (1-indexed), the spacer, then erase to end of line.
        apply_str(&mut g, b"\x1b[1;2H\x1b[K");
        let lead = g.screen().get_cell(0, 0);
        assert_eq!(lead.ch, ' ', "orphaned wide lead must be cleared too");
        assert!(!lead.wide());
    }

    #[test]
    fn resize_does_not_leave_orphan_wide_cell() {
        let mut g = Grid::new(6, 3);
        let s = format!("ab{CJK_ZHONG}de");
        apply_str(&mut g, s.as_bytes());
        g.resize(3, 5);
        for row in 0..g.screen().rows() {
            for col in 0..g.screen().cols() {
                let c = g.screen().get_cell(col, row);
                if c.wide() {
                    assert!(
                        col + 1 < g.screen().cols(),
                        "wide cell at last column with no room for spacer"
                    );
                    assert!(g.screen().get_cell(col + 1, row).spacer());
                }
                if c.spacer() {
                    assert!(col > 0);
                    assert!(g.screen().get_cell(col - 1, row).wide());
                }
            }
        }
    }
}
