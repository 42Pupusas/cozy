#![allow(clippy::missing_const_for_fn, clippy::match_same_arms, clippy::unused_self,
         clippy::needless_pass_by_ref_mut, clippy::trivially_copy_pass_by_ref,
         clippy::items_after_test_module, clippy::single_match)]
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

mod cell;
mod cursor;
mod dirty;
mod screen;

pub use cell::{Attrs, Cell, Color};
pub use cursor::{Cursor, CursorShape};
pub use dirty::{DirtyRegion, DirtySnapshot};
pub use screen::Screen;

use vt_parser::{CsiAction, Event, OscCmd, SgrAttr, C0};

/// Scrollback limit (lines above the visible area).
pub const SCROLLBACK_MAX: usize = 10_000;

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
    /// Current cursor attributes (color, shape).
    cursor: Cursor,
    /// Accumulated SGR attributes for new cells.
    attrs: Attrs,
    /// Dirty region accumulator.
    dirty: DirtyRegion,
    /// Saved cursor (DECSC).
    saved_cursor: Option<(u16, u16, Attrs)>,
    /// Bytes the terminal must write back to the PTY (CPR, DA, etc.).
    /// Drained by the driver via [`Grid::take_replies`] after each batch.
    replies: Vec<u8>,
    /// Deferred-wrap ("pending wrap" / DEC wrapnext) state. Set when a glyph
    /// fills the last column: the cursor visually stays put, and the wrap is
    /// only realized when the *next* printable char arrives. A CR/LF/cursor
    /// move clears it instead — so a newline ending a full-width line leaves
    /// that line correctly *unwrapped* (no false stitch on resize).
    pending_wrap: bool,
}

impl Grid {
    /// Create a new grid with the given dimensions.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            primary: Screen::new(cols, rows, SCROLLBACK_MAX),
            alternate: Screen::new(cols, rows, 0),
            alt_active: false,
            cursor: Cursor::default(),
            attrs: Attrs::default(),
            dirty: DirtyRegion::new(cols, rows),
            saved_cursor: None,
            replies: Vec::new(),
            pending_wrap: false,
        }
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
        let cur_col = if self.pending_wrap { self.cursor.col + 1 } else { self.cursor.col };
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
    }

    /// Apply a parsed VT event to the grid.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Print(ch) => self.print(*ch),
            Event::Control(c0) => self.control(*c0),
            Event::Csi(action) => self.csi(action),
            Event::Osc(cmd) => self.osc(cmd),
            Event::Esc { intermediate, final_byte } => {
                self.esc(*intermediate, *final_byte);
            }
            Event::Dcs | Event::Ignored => {}
        }
    }

    /// Take the current dirty snapshot, resetting the dirty accumulator.
    pub fn take_dirty(&mut self) -> DirtySnapshot {
        let screen = if self.alt_active { &self.alternate } else { &self.primary };
        self.dirty.take_snapshot(screen)
    }

    /// Take any pending reply bytes the terminal owes the host (responses to
    /// CPR/DSR/DA queries). The driver writes these back to the PTY. Empties
    /// the internal buffer.
    pub fn take_replies(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.replies)
    }

    /// Read-only view of the active screen (for testing / headless render).
    #[must_use]
    pub fn screen(&self) -> &Screen {
        self.active_screen()
    }

    /// Current cursor position.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (self.cursor.col, self.cursor.row)
    }

    // ── private ───────────────────────────────────────────────────────────

    fn active_screen(&self) -> &Screen {
        if self.alt_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_screen_mut(&mut self) -> &mut Screen {
        if self.alt_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn print(&mut self, ch: char) {
        // Realize a deferred wrap from the previous glyph: now that a real
        // printable char has arrived, the prior line genuinely continues, so
        // mark it wrapped and move to the start of the next physical row.
        if self.pending_wrap {
            let row = self.cursor.row;
            self.active_screen_mut().set_wrapped(row, true);
            self.cursor.col = 0;
            self.cursor_down(1, true);
            self.pending_wrap = false;
        }

        let col = self.cursor.col;
        let row = self.cursor.row;
        let attrs = self.attrs;
        self.active_screen_mut().set_cell(col, row, Cell { ch, attrs });
        self.dirty.mark(col, row);
        let cols = self.active_screen().cols();
        if col + 1 < cols {
            self.cursor.col += 1;
        } else {
            // Last column filled: defer the wrap. The cursor stays here; a
            // following glyph realizes the wrap (above), while a CR/LF/cursor
            // move clears the flag, leaving this line unwrapped.
            self.pending_wrap = true;
        }
    }

    fn control(&mut self, c0: C0) {
        match c0 {
            C0::Bell => { /* TODO: notify platform */ }
            C0::Backspace => {
                self.pending_wrap = false;
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            C0::Tab => {
                // Advance to next tabstop (every 8 cols).
                self.pending_wrap = false;
                let next = (self.cursor.col / 8 + 1) * 8;
                let cols = self.active_screen().cols();
                self.cursor.col = next.min(cols - 1);
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
            C0::ShiftOut | C0::ShiftIn => { /* charset switching — TODO */ }
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
                self.active_screen_mut().scroll_up(scroll_lines, top, bot);
                self.dirty.mark_rows(top, bot);
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
            CsiAction::Sgr(_)
                | CsiAction::DeviceStatusReport(_)
                | CsiAction::DeviceAttributes(_)
        ) {
            self.pending_wrap = false;
        }
        match action {
            CsiAction::CursorUp(n) => {
                self.cursor.row = self.cursor.row.saturating_sub(*n);
            }
            CsiAction::CursorDown(n) => {
                let rows = self.active_screen().rows();
                self.cursor.row = (self.cursor.row + n).min(rows - 1);
            }
            CsiAction::CursorForward(n) => {
                let cols = self.active_screen().cols();
                self.cursor.col = (self.cursor.col + n).min(cols - 1);
            }
            CsiAction::CursorBack(n) => {
                self.cursor.col = self.cursor.col.saturating_sub(*n);
            }
            CsiAction::CursorNextLine(n) => {
                self.cursor.col = 0;
                let rows = self.active_screen().rows();
                self.cursor.row = (self.cursor.row + n).min(rows - 1);
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
                self.cursor.row = row.saturating_sub(1).min(rows - 1);
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
                        // Erase entire display.
                        screen.erase_range(0, 0, cols - 1, rows - 1);
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
            CsiAction::EraseChars(n) => {
                let (col, row) = (self.cursor.col, self.cursor.row);
                let cols = self.active_screen().cols();
                let end = (col + n).min(cols);
                self.active_screen_mut().erase_line_range(row, col, end - 1);
                self.dirty.mark_row(row);
            }
            CsiAction::ScrollUp(n) => {
                let (top, bot) = self.active_screen().scroll_region();
                self.active_screen_mut().scroll_up(*n, top, bot);
                self.dirty.mark_rows(top, bot);
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
                    self.apply_sgr(attr);
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
            CsiAction::DecSet(mode) | CsiAction::SetMode(mode) => {
                self.set_mode(*mode, true);
            }
            CsiAction::DecRst(mode) | CsiAction::ResetMode(mode) => {
                self.set_mode(*mode, false);
            }
            // Ignore unhandled CSI sequences.
            CsiAction::Unknown { .. } => {}
        }
    }

    fn apply_sgr(&mut self, attr: &SgrAttr) {
        match attr {
            SgrAttr::Reset => self.attrs = Attrs::default(),
            SgrAttr::Bold => self.attrs.bold = true,
            SgrAttr::Dim => self.attrs.dim = true,
            SgrAttr::Italic => self.attrs.italic = true,
            SgrAttr::Underline => self.attrs.underline = true,
            SgrAttr::Blink | SgrAttr::RapidBlink => self.attrs.blink = true,
            SgrAttr::Inverse => self.attrs.inverse = true,
            SgrAttr::Invisible => self.attrs.invisible = true,
            SgrAttr::Strikethrough => self.attrs.strikethrough = true,
            SgrAttr::NormalIntensity => {
                self.attrs.bold = false;
                self.attrs.dim = false;
            }
            SgrAttr::NoItalic => self.attrs.italic = false,
            SgrAttr::NoUnderline => self.attrs.underline = false,
            SgrAttr::NoBlink => self.attrs.blink = false,
            SgrAttr::NoInverse => self.attrs.inverse = false,
            SgrAttr::NoInvisible => self.attrs.invisible = false,
            SgrAttr::NoStrikethrough => self.attrs.strikethrough = false,
            SgrAttr::FgStandard(n) => self.attrs.fg = Color::Indexed(*n),
            SgrAttr::BgStandard(n) => self.attrs.bg = Color::Indexed(*n),
            SgrAttr::FgDefault => self.attrs.fg = Color::Default,
            SgrAttr::BgDefault => self.attrs.bg = Color::Default,
            SgrAttr::Fg256(n) => self.attrs.fg = Color::Indexed(*n),
            SgrAttr::Bg256(n) => self.attrs.bg = Color::Indexed(*n),
            SgrAttr::FgRgb(r, g, b) => self.attrs.fg = Color::Rgb(*r, *g, *b),
            SgrAttr::BgRgb(r, g, b) => self.attrs.bg = Color::Rgb(*r, *g, *b),
        }
    }

    fn set_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            // DECTEM — cursor visibility
            25 => self.cursor.visible = enable,
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

    fn osc(&mut self, cmd: &OscCmd) {
        match cmd {
            OscCmd::SetTitle(_title) => {
                // TODO: propagate to platform window title.
            }
            _ => {}
        }
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
            }
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

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vt_parser::Parser;

    fn apply_str(grid: &mut Grid, s: &[u8]) {
        let mut p = Parser::new();
        p.feed(s, |ev| grid.apply(&ev));
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
        assert!(g.screen().get_cell(0, 0).attrs.bold);
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
        assert!(!g.screen().is_wrapped(0), "full line + newline must not wrap");
        // After resize the two lines must stay separate: NEXT begins at col 0
        // of its own row, not appended after row 0.
        g.resize(10, 4);
        assert_eq!(g.screen().get_cell(0, 1).ch, 'N');
        assert_eq!(g.screen().get_cell(0, 0).ch, '0');
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
            assert_eq!(g.screen().get_cell(i as u16, 0).ch, ch as char);
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
    fn dirty_snapshot() {
        let mut g = Grid::new(10, 3);
        apply_str(&mut g, b"X");
        let snap = g.take_dirty();
        assert!(!snap.is_empty());
        // After taking, should be clean.
        let snap2 = g.take_dirty();
        assert!(snap2.is_empty());
    }
}

// ── Display (headless render for testing) ─────────────────────────────────

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
