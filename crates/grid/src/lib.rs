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
        }
    }

    /// Resize the grid to new dimensions, reflowing soft-wrapped lines to the
    /// new width (the primary screen) and clamping the cursor.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cur = (self.cursor.col, self.cursor.row);
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
        let col = self.cursor.col;
        let row = self.cursor.row;
        let attrs = self.attrs;
        self.active_screen_mut().set_cell(col, row, Cell { ch, attrs });
        self.dirty.mark(col, row);
        let cols = self.active_screen().cols();
        if col + 1 < cols {
            self.cursor.col += 1;
        } else {
            // Auto-wrap at the right margin: the current line continues onto
            // the next physical row. Record that so resize can reflow it.
            self.active_screen_mut().set_wrapped(row, true);
            self.cursor.col = 0;
            self.cursor_down(1, true);
        }
    }

    fn control(&mut self, c0: C0) {
        match c0 {
            C0::Bell => { /* TODO: notify platform */ }
            C0::Backspace => {
                if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            C0::Tab => {
                // Advance to next tabstop (every 8 cols).
                let next = (self.cursor.col / 8 + 1) * 8;
                let cols = self.active_screen().cols();
                self.cursor.col = next.min(cols - 1);
            }
            C0::LineFeed => {
                self.cursor_down(1, true);
            }
            C0::CarriageReturn => {
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
            CsiAction::DeviceStatusReport(5) => {
                // Status OK — ignored (terminal should respond to host, handled at higher level).
            }
            CsiAction::DecSet(mode) | CsiAction::SetMode(mode) => {
                self.set_mode(*mode, true);
            }
            CsiAction::DecRst(mode) | CsiAction::ResetMode(mode) => {
                self.set_mode(*mode, false);
            }
            // Ignore unhandled CSI sequences.
            _ => {}
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
