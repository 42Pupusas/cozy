//! [`ListPane`] -- a scrollable, selectable list of rows.
//!
//! # Why this exists rather than three widgets used side by side
//!
//! [`ListViewport`], [`ListSelection`], and [`Scrollbar`] are each correct
//! and each independently tested, and *that is exactly the problem*: none of
//! them knows the others exist, so every rule that spans two of them has no
//! home. Left unowned, those rules get reimplemented -- slightly differently
//! -- in every window that shows a list:
//!
//! * Moving the cursor with an arrow key has to **scroll it into view**.
//!   The selection has no idea what is on screen; the viewport has no idea
//!   where the cursor is. Forget the call and arrowing down walks the cursor
//!   off the bottom of a window that never scrolls -- the single most common
//!   list bug there is.
//! * When the backing list shrinks (a refresh, a filter keystroke), the
//!   viewport must re-clamp its offset *and* the selection must drop stale
//!   indices. Doing one without the other leaves either a blank expanse
//!   below the last row or a highlight on a file that no longer exists.
//! * A click has to be routed: the scrollbar gutter and the rows share a
//!   coordinate space, and whichever is asked first wins.
//!
//! So this type owns the *coupling*, and nothing else. It holds no pixels,
//! no fonts, and no protocol -- painting is the caller's job, driven by
//! [`Self::visible_rows`] and [`Self::row_y`]. That keeps the interaction
//! rules testable in microseconds, which is the whole reason this crate has
//! no dependencies.

use crate::command::{Edge, ListCommand, Mods, PaneResponse};
use crate::scrollbar::Scrollbar;
use crate::selection::ListSelection;
use crate::viewport::ListViewport;

/// A scrollable, selectable list: viewport + selection + scrollbar, with the
/// rules that span them.
#[derive(Debug, Clone)]
pub struct ListPane {
    viewport: ListViewport,
    selection: ListSelection,
    scrollbar: Scrollbar,
    /// Full pane width in pixels, including the scrollbar gutter. Needed
    /// because a click's *x* is what decides whether it is a row click or a
    /// scrollbar grab.
    width: i32,
    /// Width of the scrollbar gutter on the right edge.
    gutter: i32,
}

impl ListPane {
    /// Default scrollbar gutter width in pixels -- wide enough to grab
    /// without swallowing a meaningful slice of a narrow window.
    pub const DEFAULT_GUTTER: i32 = 12;

    /// A pane of `width` x `height` pixels showing `len` rows of
    /// `row_height` pixels each.
    #[must_use]
    pub fn new(width: i32, height: i32, row_height: i32, len: usize) -> Self {
        Self {
            viewport: ListViewport::new(row_height, len, height),
            selection: ListSelection::new(),
            scrollbar: Scrollbar::new(height),
            width: width.max(0),
            gutter: Self::DEFAULT_GUTTER,
        }
    }

    /// Overrides the scrollbar gutter width (builder style).
    #[must_use]
    pub fn with_gutter(mut self, gutter: i32) -> Self {
        self.gutter = gutter.max(0);
        self
    }

    /// The scroll viewport, for painting and hit-testing.
    #[must_use]
    pub const fn viewport(&self) -> &ListViewport {
        &self.viewport
    }

    /// The current selection, for painting row highlights.
    #[must_use]
    pub const fn selection(&self) -> &ListSelection {
        &self.selection
    }

    /// The scrollbar, for painting its thumb.
    #[must_use]
    pub const fn scrollbar(&self) -> &Scrollbar {
        &self.scrollbar
    }

    /// Whether the scrollbar should be drawn at all.
    #[must_use]
    pub fn scrollbar_visible(&self) -> bool {
        self.scrollbar.is_needed(&self.viewport)
    }

    /// The x coordinate where the scrollbar gutter starts.
    #[must_use]
    pub const fn gutter_x(&self) -> i32 {
        self.width - self.gutter
    }

    /// Width available for row content, excluding the gutter when a
    /// scrollbar is actually shown.
    ///
    /// Reserving the gutter unconditionally would make text reflow every
    /// time a list crossed the overflow threshold; reserving it only when
    /// visible means a short list uses the full width.
    #[must_use]
    pub fn content_width(&self) -> i32 {
        if self.scrollbar_visible() {
            self.gutter_x().max(0)
        } else {
            self.width
        }
    }

    /// The half-open range of row indices at least partly on screen.
    #[must_use]
    pub fn visible_rows(&self) -> core::ops::Range<usize> {
        self.viewport.visible_rows()
    }

    /// Where to paint `row`, relative to the top of the pane.
    #[must_use]
    pub fn row_y(&self, row: usize) -> Option<i32> {
        self.viewport.row_y(row)
    }

    /// The cursor row, if any.
    #[must_use]
    pub const fn cursor(&self) -> Option<usize> {
        self.selection.cursor()
    }

    /// Whether `row` is selected -- what a painter asks per visible row.
    #[must_use]
    pub fn is_selected(&self, row: usize) -> bool {
        self.selection.contains(row)
    }

    /// How many rows the list holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.viewport.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.viewport.is_empty()
    }

    /// Resize the pane, e.g. on a window configure.
    ///
    /// The scrollbar track always matches the viewport height; letting them
    /// drift is what produces a thumb that cannot reach the bottom.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.width = width.max(0);
        self.viewport.set_height(height);
        self.scrollbar.set_track(self.viewport.height());
    }

    /// Replace the row count after the backing list changed.
    ///
    /// Both halves are mandatory and are the reason this lives here: the
    /// viewport re-clamps a stranded offset, and the selection drops indices
    /// that now point past the end. Doing one without the other is the
    /// stale-highlight bug described in the module docs.
    pub fn set_len(&mut self, len: usize) {
        self.viewport.set_len(len);
        self.selection.retain_below(len);
        // A cursor that survived may now be off screen -- e.g. the list
        // shrank around it -- so re-establish the invariant that the cursor
        // is always visible.
        if let Some(cursor) = self.selection.cursor() {
            self.viewport.scroll_into_view(cursor);
        }
    }

    /// Point the pane at a fresh list: new length, nothing selected, scrolled
    /// back to the top.
    ///
    /// What navigating to another directory calls. Distinct from
    /// [`Self::set_len`], which preserves position for a refresh *of the same
    /// list* -- a new file appearing must not fling the user to the top, but
    /// entering a new directory part-way down would be baffling.
    pub fn reset(&mut self, len: usize) {
        self.viewport.set_len(len);
        self.viewport.scroll_to(0);
        self.selection.clear();
    }

    /// How many rows a page jump moves.
    ///
    /// One less than a screenful, so the row at the boundary stays visible
    /// across the jump -- the same overlap rule the viewport uses for pixel
    /// paging, applied to the cursor. Never zero, or Page Down would do
    /// nothing in a very short window.
    #[must_use]
    fn rows_per_page(&self) -> i32 {
        let rows = self.viewport.height() / self.viewport.row_height();
        (rows - 1).max(1)
    }

    /// Apply a semantic command.
    pub fn command(&mut self, cmd: ListCommand) -> PaneResponse {
        let len = self.viewport.len();
        match cmd {
            ListCommand::Move(dir) => self.move_cursor(dir.delta(), len, false),
            ListCommand::Extend(dir) => self.move_cursor(dir.delta(), len, true),
            ListCommand::Page(dir) => {
                let delta = dir.delta().saturating_mul(self.rows_per_page());
                self.move_cursor(delta, len, false)
            }
            ListCommand::Jump(edge) => self.jump(edge, len),
            ListCommand::SelectAll => {
                if len == 0 {
                    return PaneResponse::Ignored;
                }
                self.selection.select_all(len);
                PaneResponse::Redraw
            }
            ListCommand::ClearSelection => {
                if self.selection.is_empty() && self.selection.cursor().is_none() {
                    return PaneResponse::Ignored;
                }
                self.selection.clear();
                PaneResponse::Redraw
            }
            ListCommand::Activate => self
                .selection
                .cursor()
                .map_or(PaneResponse::Ignored, PaneResponse::Activate),
            ListCommand::Scroll(lines) => {
                let delta = lines.saturating_mul(self.viewport.row_height());
                PaneResponse::redraw_if(self.viewport.scroll_by(delta))
            }
        }
    }

    /// Put the cursor on `row`, replacing the selection, and scroll it into
    /// view. Ignores an out-of-range index.
    ///
    /// The programmatic entry point -- what "go up and land on the directory
    /// you just left" and "keep the cursor on the same file across a refresh"
    /// both need. It goes through the pane rather than the selection directly
    /// precisely so the scroll happens: landing on an off-screen cursor is
    /// indistinguishable from not having landed anywhere.
    pub fn select_row(&mut self, row: usize) -> bool {
        if row >= self.viewport.len() {
            return false;
        }
        self.selection.select_only(row);
        self.viewport.scroll_into_view(row);
        true
    }

    /// Jump the cursor to one end of the list.
    ///
    /// # Why this names its target instead of reusing a huge delta
    ///
    /// The tempting shortcut is to route both edges through
    /// [`Self::move_cursor`] with `i32::MIN` / `i32::MAX`, letting the
    /// existing clamp do the work. That is wrong, and silently so: when
    /// there is no cursor yet, `ListSelection::move_cursor` ignores the
    /// magnitude entirely and branches on the *sign* -- a downward move
    /// starts at the first row, an upward one at the last, which is what
    /// makes the very first arrow press sensible. Feed it `i32::MAX` and
    /// Home/End come out exactly inverted on a fresh window.
    fn jump(&mut self, edge: Edge, len: usize) -> PaneResponse {
        if len == 0 {
            return PaneResponse::Ignored;
        }
        let target = match edge {
            Edge::Top => 0,
            Edge::Bottom => len - 1,
        };
        let before = self.selection.cursor();
        self.selection.select_only(target);
        let scrolled = self.viewport.scroll_into_view(target);
        PaneResponse::redraw_if(scrolled || before != Some(target))
    }

    /// Move the cursor and keep it on screen.
    ///
    /// The scroll-into-view call is the single most important line in this
    /// type: without it, arrowing past the bottom row moves an invisible
    /// cursor and the list appears frozen.
    fn move_cursor(&mut self, delta: i32, len: usize, extending: bool) -> PaneResponse {
        let before = self.selection.cursor();
        let moved = if extending {
            self.selection.move_cursor_extending(delta, len)
        } else {
            self.selection.move_cursor(delta, len)
        };
        let Some(cursor) = moved else {
            // An empty list: the selection cleared itself. Only a repaint if
            // there was something to clear.
            return PaneResponse::redraw_if(before.is_some());
        };
        let scrolled = self.viewport.scroll_into_view(cursor);
        // A repaint is owed if the cursor moved *or* the view did -- and also
        // when a fresh cursor appeared at the same index it would have had,
        // since the highlight is new even though the number is unchanged.
        PaneResponse::redraw_if(scrolled || before != moved || before.is_none())
    }

    /// A pointer press at pane-relative `x`, `y`.
    ///
    /// Routed by x: the gutter takes the press when a scrollbar is shown,
    /// otherwise it is a row click. Clicking below the last row clears the
    /// selection, which is the standard way to deselect everything.
    pub fn press(&mut self, x: i32, y: i32, mods: Mods) -> PaneResponse {
        if self.scrollbar_visible() && x >= self.gutter_x() {
            return PaneResponse::redraw_if(self.scrollbar.press(&mut self.viewport, y));
        }
        let Some(row) = self.viewport.row_at_y(y) else {
            // Empty space below the rows.
            if self.selection.is_empty() && self.selection.cursor().is_none() {
                return PaneResponse::Ignored;
            }
            self.selection.clear();
            return PaneResponse::Redraw;
        };
        // Shift is checked before Ctrl: with both held, extending is the
        // behaviour every file manager settles on, and a toggle-then-extend
        // interpretation would leave the anchor somewhere the user never put
        // it.
        if mods.shift {
            self.selection.extend_to(row);
        } else if mods.ctrl {
            self.selection.toggle(row);
        } else {
            self.selection.select_only(row);
        }
        self.viewport.scroll_into_view(row);
        PaneResponse::Redraw
    }

    /// Replace the selection while retaining a valid focused row.
    ///
    /// This is intentionally the narrow transfer seam for a second view of
    /// the same directory: selection is user state, not list-layout state.
    pub fn replace_selection(&mut self, selected: &[usize], cursor: Option<usize>) {
        self.selection.clear();
        for &row in selected {
            if row < self.viewport.len() {
                self.selection.toggle(row);
            }
        }
        if let Some(row) = cursor.filter(|&row| row < self.viewport.len()) {
            self.selection.select_only(row);
            self.viewport.scroll_into_view(row);
        }
    }

    /// Pointer motion. Only meaningful mid-scrollbar-drag; row hover is the
    /// caller's business, since it needs no state this type owns.
    pub fn motion(&mut self, y: i32) -> PaneResponse {
        if !self.scrollbar.is_dragging() {
            return PaneResponse::Ignored;
        }
        PaneResponse::redraw_if(self.scrollbar.drag(&mut self.viewport, y))
    }

    /// A pointer release, ending any scrollbar drag.
    pub const fn release(&mut self) -> PaneResponse {
        let was_dragging = self.scrollbar.is_dragging();
        self.scrollbar.release();
        // The thumb is painted differently while held on most themes, so
        // ending a drag is itself a visible change.
        PaneResponse::redraw_if(was_dragging)
    }

    /// A double-click at pane-relative `x`, `y` -- select the row and
    /// activate it.
    pub fn double_click(&mut self, x: i32, y: i32) -> PaneResponse {
        if self.scrollbar_visible() && x >= self.gutter_x() {
            return PaneResponse::Ignored;
        }
        match self.viewport.row_at_y(y) {
            Some(row) => {
                self.selection.select_only(row);
                PaneResponse::Activate(row)
            }
            None => PaneResponse::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ListPane;
    use crate::command::{Direction, Edge, ListCommand, Mods, PaneResponse};

    /// 400x200 pane, 20px rows (10 rows fit), 50 rows of content.
    fn pane() -> ListPane {
        ListPane::new(400, 200, 20, 50)
    }

    /// A pane whose content fits entirely, so no scrollbar is needed.
    fn short_pane() -> ListPane {
        ListPane::new(400, 200, 20, 3)
    }

    /// The invariant this whole type exists to maintain.
    fn assert_cursor_visible(pane: &ListPane) {
        if let Some(cursor) = pane.cursor() {
            let visible = pane.visible_rows();
            assert!(
                visible.contains(&cursor),
                "cursor {cursor} outside visible {visible:?} (offset {})",
                pane.viewport().offset()
            );
        }
    }

    #[test]
    fn a_fresh_pane_is_at_the_top_with_nothing_selected() {
        let pane = pane();
        assert_eq!(pane.cursor(), None);
        assert_eq!(pane.viewport().offset(), 0);
        assert_eq!(pane.len(), 50);
        assert!(!pane.is_empty());
    }

    #[test]
    fn the_first_down_press_lands_on_the_first_row() {
        let mut pane = pane();
        assert_eq!(
            pane.command(ListCommand::Move(Direction::Down)),
            PaneResponse::Redraw
        );
        assert_eq!(pane.cursor(), Some(0));
        assert_cursor_visible(&pane);
    }

    /// The headline bug: without a scroll-into-view the cursor walks off the
    /// bottom and the list looks frozen.
    #[test]
    fn arrowing_past_the_bottom_row_scrolls_the_view() {
        let mut pane = pane();
        for _ in 0..15 {
            pane.command(ListCommand::Move(Direction::Down));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(14));
        assert!(
            pane.viewport().offset() > 0,
            "the view must have followed the cursor down"
        );
    }

    /// And back the other way, which is a separate branch in the viewport.
    #[test]
    fn arrowing_back_above_the_top_row_scrolls_the_view_up() {
        let mut pane = pane();
        for _ in 0..20 {
            pane.command(ListCommand::Move(Direction::Down));
        }
        let deep = pane.viewport().offset();
        assert!(deep > 0);
        for _ in 0..20 {
            pane.command(ListCommand::Move(Direction::Up));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(0));
        assert_eq!(pane.viewport().offset(), 0);
    }

    /// Walking the entire list in both directions must never break the
    /// invariant at any point -- derived from real geometry, not a spot check.
    #[test]
    fn walking_the_whole_list_keeps_the_cursor_visible_throughout() {
        let mut pane = pane();
        for _ in 0..60 {
            pane.command(ListCommand::Move(Direction::Down));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(49), "clamped at the last row");
        for _ in 0..60 {
            pane.command(ListCommand::Move(Direction::Up));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(0));
    }

    #[test]
    fn a_move_that_changes_nothing_reports_ignored() {
        let mut pane = pane();
        pane.command(ListCommand::Move(Direction::Up)); // lands on the last row
        assert_eq!(pane.cursor(), Some(49));
        assert_eq!(
            pane.command(ListCommand::Move(Direction::Down)),
            PaneResponse::Ignored,
            "already hard against the end: no repaint owed"
        );
    }

    #[test]
    fn shift_arrow_extends_the_selection() {
        let mut pane = pane();
        pane.command(ListCommand::Move(Direction::Down));
        pane.command(ListCommand::Extend(Direction::Down));
        pane.command(ListCommand::Extend(Direction::Down));
        assert_eq!(pane.cursor(), Some(2));
        assert!(pane.is_selected(0) && pane.is_selected(1) && pane.is_selected(2));
        assert_eq!(pane.selection().count(), 3);
    }

    /// A page jump moves a screenful less one row, so the boundary row stays
    /// on screen. Derived from the pane's real geometry rather than hardcoded.
    #[test]
    fn paging_down_moves_a_screenful_less_one_row() {
        let mut pane = pane();
        let rows_on_screen = pane.viewport().height() / pane.viewport().row_height();
        let expected_step = usize::try_from(rows_on_screen - 1).expect("positive");

        pane.command(ListCommand::Move(Direction::Down)); // cursor at 0
        pane.command(ListCommand::Page(Direction::Down));
        assert_eq!(pane.cursor(), Some(expected_step));
        assert_cursor_visible(&pane);
    }

    #[test]
    fn paging_clamps_at_both_ends() {
        let mut pane = pane();
        for _ in 0..20 {
            pane.command(ListCommand::Page(Direction::Down));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(49));
        for _ in 0..20 {
            pane.command(ListCommand::Page(Direction::Up));
            assert_cursor_visible(&pane);
        }
        assert_eq!(pane.cursor(), Some(0));
    }

    /// A window so short that a page would otherwise be zero rows must still
    /// advance, or Page Down would silently do nothing.
    #[test]
    fn paging_in_a_pane_shorter_than_a_row_still_advances() {
        let mut pane = ListPane::new(400, 10, 20, 50);
        pane.command(ListCommand::Move(Direction::Down));
        pane.command(ListCommand::Page(Direction::Down));
        assert_eq!(pane.cursor(), Some(1));
    }

    #[test]
    fn jumping_reaches_both_ends_and_scrolls_there() {
        let mut pane = pane();
        pane.command(ListCommand::Jump(Edge::Bottom));
        assert_eq!(pane.cursor(), Some(49));
        assert_eq!(pane.viewport().offset(), pane.viewport().max_scroll());
        assert_cursor_visible(&pane);

        pane.command(ListCommand::Jump(Edge::Top));
        assert_eq!(pane.cursor(), Some(0));
        assert_eq!(pane.viewport().offset(), 0);
    }

    #[test]
    fn selecting_a_row_programmatically_scrolls_it_into_view() {
        let mut pane = pane();
        assert!(pane.select_row(40));
        assert_eq!(pane.cursor(), Some(40));
        assert_eq!(pane.selection().count(), 1);
        assert_cursor_visible(&pane);
    }

    #[test]
    fn selecting_an_out_of_range_row_is_refused_rather_than_panicking() {
        let mut pane = pane();
        assert!(!pane.select_row(50), "one past the end");
        assert!(!pane.select_row(usize::MAX));
        assert_eq!(
            pane.cursor(),
            None,
            "a refused select must not move anything"
        );
    }

    #[test]
    fn select_all_covers_the_list_and_clear_empties_it() {
        let mut pane = pane();
        assert_eq!(pane.command(ListCommand::SelectAll), PaneResponse::Redraw);
        assert_eq!(pane.selection().count(), 50);
        assert_eq!(
            pane.command(ListCommand::ClearSelection),
            PaneResponse::Redraw
        );
        assert!(pane.selection().is_empty());
        assert_eq!(
            pane.command(ListCommand::ClearSelection),
            PaneResponse::Ignored,
            "clearing an empty selection changes nothing"
        );
    }

    #[test]
    fn activating_reports_the_cursor_row() {
        let mut pane = pane();
        assert_eq!(
            pane.command(ListCommand::Activate),
            PaneResponse::Ignored,
            "nothing to activate without a cursor"
        );
        pane.command(ListCommand::Jump(Edge::Bottom));
        assert_eq!(
            pane.command(ListCommand::Activate),
            PaneResponse::Activate(49)
        );
    }

    #[test]
    fn scrolling_moves_the_view_without_moving_the_cursor() {
        let mut pane = pane();
        pane.command(ListCommand::Move(Direction::Down));
        let cursor = pane.cursor();
        assert_eq!(pane.command(ListCommand::Scroll(3)), PaneResponse::Redraw);
        assert_eq!(pane.viewport().offset(), 3 * pane.viewport().row_height());
        assert_eq!(
            pane.cursor(),
            cursor,
            "a wheel scroll must not move the cursor"
        );
    }

    #[test]
    fn scrolling_against_a_stop_reports_ignored() {
        let mut pane = pane();
        assert_eq!(pane.command(ListCommand::Scroll(-1)), PaneResponse::Ignored);
    }

    // -- empty lists ------------------------------------------------------

    #[test]
    fn every_command_on_an_empty_list_is_harmless() {
        let mut pane = ListPane::new(400, 200, 20, 0);
        assert!(pane.is_empty());
        for cmd in [
            ListCommand::Move(Direction::Down),
            ListCommand::Move(Direction::Up),
            ListCommand::Extend(Direction::Down),
            ListCommand::Page(Direction::Down),
            ListCommand::Jump(Edge::Top),
            ListCommand::Jump(Edge::Bottom),
            ListCommand::SelectAll,
            ListCommand::ClearSelection,
            ListCommand::Activate,
            ListCommand::Scroll(5),
        ] {
            assert_eq!(pane.command(cmd), PaneResponse::Ignored, "{cmd:?}");
        }
        assert_eq!(pane.cursor(), None);
    }

    // -- clicks -----------------------------------------------------------

    #[test]
    fn clicking_a_row_selects_it() {
        let mut pane = pane();
        // Derived from geometry: the middle of row 3.
        let y = 3 * pane.viewport().row_height() + pane.viewport().row_height() / 2;
        assert_eq!(pane.press(10, y, Mods::none()), PaneResponse::Redraw);
        assert_eq!(pane.cursor(), Some(3));
        assert_eq!(pane.selection().count(), 1);
    }

    #[test]
    fn ctrl_click_toggles_without_dropping_the_rest() {
        let mut pane = pane();
        let row_h = pane.viewport().row_height();
        pane.press(10, row_h / 2, Mods::none()); // row 0
        pane.press(10, 2 * row_h + row_h / 2, Mods::ctrl()); // row 2
        assert!(pane.is_selected(0) && pane.is_selected(2));
        assert_eq!(pane.selection().count(), 2);

        pane.press(10, 2 * row_h + row_h / 2, Mods::ctrl());
        assert!(!pane.is_selected(2), "a second ctrl-click deselects");
        assert_eq!(pane.selection().count(), 1);
    }

    #[test]
    fn shift_click_selects_the_span() {
        let mut pane = pane();
        let row_h = pane.viewport().row_height();
        pane.press(10, row_h / 2, Mods::none()); // row 0
        pane.press(10, 4 * row_h + row_h / 2, Mods::shift()); // row 4
        assert_eq!(pane.selection().count(), 5);
        for row in 0..=4 {
            assert!(pane.is_selected(row), "row {row} in the span");
        }
    }

    /// With both modifiers held, extending wins -- and the point of the test
    /// is that the *toggle* did not also happen.
    #[test]
    fn shift_takes_precedence_over_ctrl() {
        let mut pane = pane();
        let row_h = pane.viewport().row_height();
        pane.press(10, row_h / 2, Mods::none());
        pane.press(
            10,
            3 * row_h + row_h / 2,
            Mods {
                ctrl: true,
                shift: true,
            },
        );
        assert_eq!(pane.selection().count(), 4, "extended, not toggled");
    }

    #[test]
    fn clicking_empty_space_below_a_short_list_clears_the_selection() {
        let mut pane = short_pane();
        pane.press(10, pane.viewport().row_height() / 2, Mods::none());
        assert_eq!(pane.selection().count(), 1);
        // Well below the 3 rows of content, but inside the pane.
        assert_eq!(pane.press(10, 180, Mods::none()), PaneResponse::Redraw);
        assert!(pane.selection().is_empty());
        assert_eq!(
            pane.press(10, 180, Mods::none()),
            PaneResponse::Ignored,
            "clearing an already-empty selection is not a repaint"
        );
    }

    #[test]
    fn a_double_click_selects_and_activates() {
        let mut pane = pane();
        let row_h = pane.viewport().row_height();
        assert_eq!(
            pane.double_click(10, 5 * row_h + row_h / 2),
            PaneResponse::Activate(5)
        );
        assert!(pane.is_selected(5));
    }

    #[test]
    fn a_double_click_on_empty_space_activates_nothing() {
        let mut pane = short_pane();
        assert_eq!(pane.double_click(10, 180), PaneResponse::Ignored);
    }

    // -- the scrollbar gutter ---------------------------------------------

    #[test]
    fn a_short_list_shows_no_scrollbar_and_uses_the_full_width() {
        let pane = short_pane();
        assert!(!pane.scrollbar_visible());
        assert_eq!(pane.content_width(), 400);
    }

    #[test]
    fn an_overflowing_list_reserves_the_gutter() {
        let pane = pane();
        assert!(pane.scrollbar_visible());
        assert_eq!(pane.content_width(), 400 - ListPane::DEFAULT_GUTTER);
    }

    /// A click in the gutter must scroll, not select -- the routing rule.
    #[test]
    fn clicking_the_gutter_scrolls_rather_than_selecting() {
        let mut pane = pane();
        // Below the thumb, so the press jumps the view down.
        let x = pane.gutter_x() + 2;
        assert_eq!(pane.press(x, 150, Mods::none()), PaneResponse::Redraw);
        assert!(pane.viewport().offset() > 0);
        assert!(
            pane.selection().is_empty(),
            "a gutter press must not select a row"
        );
    }

    /// The same x on a list that does not overflow is an ordinary row click:
    /// there is no scrollbar there to hit.
    #[test]
    fn the_gutter_x_is_a_row_click_when_no_scrollbar_is_shown() {
        let mut pane = short_pane();
        let x = pane.gutter_x() + 2;
        pane.press(x, pane.viewport().row_height() / 2, Mods::none());
        assert_eq!(pane.cursor(), Some(0));
    }

    #[test]
    fn dragging_the_thumb_scrolls_and_releasing_ends_it() {
        let mut pane = pane();
        let x = pane.gutter_x() + 2;
        pane.press(x, 5, Mods::none()); // grabs near the top
        assert!(pane.scrollbar().is_dragging());
        assert_eq!(pane.motion(120), PaneResponse::Redraw);
        assert!(pane.viewport().offset() > 0);
        assert_eq!(pane.release(), PaneResponse::Redraw);
        assert!(!pane.scrollbar().is_dragging());
        assert_eq!(
            pane.motion(40),
            PaneResponse::Ignored,
            "motion after release must not scroll"
        );
    }

    #[test]
    fn a_release_without_a_drag_is_not_a_repaint() {
        let mut pane = pane();
        assert_eq!(pane.release(), PaneResponse::Ignored);
    }

    /// Dragging the thumb hard to the bottom must reach the true end of the
    /// list -- the travel-vs-track division bug, checked through the pane.
    #[test]
    fn dragging_to_the_bottom_reaches_the_last_row() {
        let mut pane = pane();
        let x = pane.gutter_x() + 2;
        pane.press(x, 5, Mods::none());
        pane.motion(10_000);
        assert_eq!(pane.viewport().offset(), pane.viewport().max_scroll());
        let visible = pane.visible_rows();
        assert!(visible.contains(&49), "the last row must be on screen");
    }

    // -- resizing and list changes ----------------------------------------

    #[test]
    fn resizing_keeps_the_scrollbar_track_matched_to_the_viewport() {
        let mut pane = pane();
        pane.resize(300, 400);
        assert_eq!(pane.viewport().height(), 400);
        assert_eq!(pane.scrollbar().track(), 400);
        assert_eq!(pane.gutter_x(), 300 - ListPane::DEFAULT_GUTTER);
    }

    /// Growing the window can strand the offset past the new maximum.
    #[test]
    fn growing_the_pane_pulls_a_stranded_offset_back() {
        let mut pane = pane();
        pane.command(ListCommand::Jump(Edge::Bottom));
        pane.resize(400, 2000);
        assert!(pane.viewport().offset() <= pane.viewport().max_scroll());
        assert_cursor_visible(&pane);
    }

    /// The other half of the coupling: a shrinking list must not leave a
    /// highlight on a row that no longer exists.
    #[test]
    fn shrinking_the_list_drops_stale_selection_and_reclamps() {
        let mut pane = pane();
        pane.command(ListCommand::SelectAll);
        pane.command(ListCommand::Jump(Edge::Bottom));
        pane.set_len(5);
        assert_eq!(pane.len(), 5);
        assert!(
            pane.selection().selected().iter().all(|&r| r < 5),
            "no index may point past the end"
        );
        assert!(pane.cursor().is_some_and(|c| c < 5));
        assert!(pane.viewport().offset() <= pane.viewport().max_scroll());
        assert_cursor_visible(&pane);
    }

    /// A refresh that adds a file must not fling the user back to the top.
    #[test]
    fn growing_the_list_keeps_the_scroll_position() {
        let mut pane = pane();
        pane.command(ListCommand::Scroll(10));
        let offset = pane.viewport().offset();
        pane.set_len(60);
        assert_eq!(pane.viewport().offset(), offset);
    }

    /// Navigating to a new directory, by contrast, starts fresh.
    #[test]
    fn reset_returns_to_the_top_with_nothing_selected() {
        let mut pane = pane();
        pane.command(ListCommand::SelectAll);
        pane.command(ListCommand::Jump(Edge::Bottom));
        pane.reset(30);
        assert_eq!(pane.len(), 30);
        assert_eq!(pane.viewport().offset(), 0);
        assert_eq!(pane.cursor(), None);
        assert!(pane.selection().is_empty());
    }

    /// Shrinking to nothing is the degenerate case that tends to panic.
    #[test]
    fn shrinking_to_an_empty_list_leaves_no_cursor() {
        let mut pane = pane();
        pane.command(ListCommand::SelectAll);
        pane.set_len(0);
        assert_eq!(pane.cursor(), None);
        assert!(pane.selection().is_empty());
        assert!(pane.is_empty());
        assert_eq!(pane.visible_rows(), 0..0);
    }

    /// A long mixed session: the structural invariants must hold after every
    /// single interaction, not just at the end.
    ///
    /// Note what is *not* asserted here: that the cursor stays visible.
    /// Scrolling the wheel and dragging the thumb move the view **without**
    /// moving the cursor -- deliberately, and
    /// `scrolling_moves_the_view_without_moving_the_cursor` pins that down --
    /// so the cursor is legitimately off screen afterwards. Cursor visibility
    /// is a postcondition of cursor *movement*, checked by the tests that
    /// move it; asserting it here would be asserting the opposite of the
    /// documented behaviour.
    #[test]
    fn a_realistic_session_holds_the_invariant_throughout() {
        let mut pane = pane();
        let row_h = pane.viewport().row_height();
        let x = pane.gutter_x() + 2;

        for step in 0..40 {
            let moved_cursor = matches!(step % 8, 0 | 1 | 2 | 3 | 7);
            match step % 8 {
                0 => drop(pane.command(ListCommand::Move(Direction::Down))),
                1 => drop(pane.command(ListCommand::Page(Direction::Down))),
                2 => drop(pane.command(ListCommand::Extend(Direction::Up))),
                3 => drop(pane.press(10, (step % 10) * row_h + 1, Mods::none())),
                4 => drop(pane.command(ListCommand::Scroll(step - 20))),
                5 => {
                    pane.press(x, step * 3, Mods::none());
                    pane.motion(step * 5);
                    pane.release();
                }
                6 => pane.set_len(50 - usize::try_from(step % 7).expect("small")),
                _ => drop(pane.command(ListCommand::Jump(Edge::Bottom))),
            }
            if moved_cursor {
                assert_cursor_visible(&pane);
            }
            assert!(
                pane.viewport().offset() >= 0
                    && pane.viewport().offset() <= pane.viewport().max_scroll(),
                "offset out of range at step {step}"
            );
            assert!(
                pane.selection().selected().iter().all(|&r| r < pane.len()),
                "stale selection index at step {step}"
            );
        }
    }

    /// The converse of the session test: a cursor pushed off screen by a
    /// wheel scroll is pulled back the moment the user touches an arrow key.
    #[test]
    fn a_cursor_scrolled_off_screen_returns_on_the_next_arrow_press() {
        let mut pane = pane();
        pane.command(ListCommand::Move(Direction::Down));
        pane.command(ListCommand::Scroll(30));
        assert!(
            !pane.visible_rows().contains(&0),
            "the wheel scrolled the cursor row out of view"
        );
        pane.command(ListCommand::Move(Direction::Down));
        assert_cursor_visible(&pane);
    }
}
