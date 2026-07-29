//! [`GridPane`] -- a scrollable, selectable uniform-card grid.
//!
//! This is deliberately a sibling of [`crate::ListPane`], not a rendering
//! mode bolted onto it. A list scrolls and pages in *items*; a grid scrolls
//! in card rows and its vertical cursor movement must preserve a column. The
//! shared low-level pieces (`ListViewport`, `ListSelection`, `Scrollbar`) are
//! reused, while this type owns the rules that genuinely differ.

use crate::{
    Direction, Edge, ListCommand, ListSelection, ListViewport, Mods, PaneResponse, Scrollbar,
};

/// Pixel geometry of a visible card, relative to a grid pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCell {
    /// Index in the backing item list.
    pub index: usize,
    /// Card left edge, relative to the pane.
    pub x: i32,
    /// Card top edge, relative to the pane. It may be negative while scrolling.
    pub y: i32,
    /// Card width.
    pub width: i32,
    /// Card height.
    pub height: i32,
}

/// A selectable grid of equal-size cards.
#[derive(Debug, Clone)]
pub struct GridPane {
    viewport: ListViewport,
    selection: ListSelection,
    scrollbar: Scrollbar,
    width: i32,
    card_width: i32,
    card_height: i32,
    gap: i32,
    gutter: i32,
    /// Number of real items. The viewport counts visual card rows instead.
    len: usize,
}

impl GridPane {
    /// The scrollbar gutter shared with [`crate::ListPane`].
    pub const DEFAULT_GUTTER: i32 = 12;

    /// Build a grid whose cards are `card_width` by `card_height` pixels.
    #[must_use]
    pub fn new(
        width: i32,
        height: i32,
        card_width: i32,
        card_height: i32,
        gap: i32,
        len: usize,
    ) -> Self {
        let mut pane = Self {
            viewport: ListViewport::new(card_height.max(1), 0, height),
            selection: ListSelection::new(),
            scrollbar: Scrollbar::new(height),
            width: width.max(0),
            card_width: card_width.max(1),
            card_height: card_height.max(1),
            gap: gap.max(0),
            gutter: Self::DEFAULT_GUTTER,
            len: 0,
        };
        pane.set_len(len);
        pane
    }

    /// The grid viewport, expressed in card rows rather than item rows.
    #[must_use]
    pub const fn viewport(&self) -> &ListViewport {
        &self.viewport
    }

    /// The current selection.
    #[must_use]
    pub const fn selection(&self) -> &ListSelection {
        &self.selection
    }

    /// The grid's scrollbar.
    #[must_use]
    pub const fn scrollbar(&self) -> &Scrollbar {
        &self.scrollbar
    }

    /// The focused item, if any.
    #[must_use]
    pub const fn cursor(&self) -> Option<usize> {
        self.selection.cursor()
    }

    /// Whether one item is selected.
    #[must_use]
    pub fn is_selected(&self, item: usize) -> bool {
        self.selection.contains(item)
    }

    /// Number of items in the grid.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the grid has no cards.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of cards in one visual row.
    #[must_use]
    pub fn columns(&self) -> usize {
        let content = self.content_width();
        let stride = self.card_width.saturating_add(self.gap).max(1);
        usize::try_from((content.saturating_add(self.gap) / stride).max(1)).unwrap_or(1)
    }

    /// The width usable by cards, excluding a scrollbar only when needed.
    #[must_use]
    pub fn content_width(&self) -> i32 {
        if self.scrollbar_visible() {
            (self.width - self.gutter).max(0)
        } else {
            self.width
        }
    }

    /// Whether the grid's content overflows vertically.
    #[must_use]
    pub fn scrollbar_visible(&self) -> bool {
        self.scrollbar.is_needed(&self.viewport)
    }

    /// The scrollbar's left edge in pane-relative coordinates.
    #[must_use]
    pub const fn gutter_x(&self) -> i32 {
        self.width - self.gutter
    }

    /// Resize the visible grid area.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.width = width.max(0);
        self.viewport.set_height(height);
        self.scrollbar.set_track(self.viewport.height());
        self.relength();
    }

    /// Replace the item count, preserving any still-valid cursor and scroll.
    pub fn set_len(&mut self, len: usize) {
        self.set_item_len(len);
    }

    /// Reset for a newly opened directory.
    pub fn reset(&mut self, len: usize) {
        self.set_item_len(len);
        self.viewport.scroll_to(0);
        self.selection.clear();
    }

    /// Return every card at least partly visible in the viewport.
    #[must_use]
    pub fn visible_cells(&self) -> Vec<GridCell> {
        let columns = self.columns();
        let item_len = self.len;
        self.viewport
            .visible_rows()
            .flat_map(|row| {
                let y = self.viewport.row_y(row).unwrap_or(0);
                (0..columns).filter_map(move |column| {
                    let index = row.saturating_mul(columns).saturating_add(column);
                    (index < item_len).then_some(GridCell {
                        index,
                        x: i32::try_from(column)
                            .unwrap_or(i32::MAX)
                            .saturating_mul(self.card_width.saturating_add(self.gap)),
                        y,
                        width: self.card_width,
                        height: self.card_height,
                    })
                })
            })
            .collect()
    }

    /// Apply a command using two-dimensional cursor movement.
    pub fn command(&mut self, command: ListCommand) -> PaneResponse {
        let len = self.len;
        match command {
            ListCommand::Move(direction) => self.move_cursor(direction, len, false),
            ListCommand::Extend(direction) => self.move_cursor(direction, len, true),
            ListCommand::Page(direction) => self.page(direction, len),
            ListCommand::Jump(edge) => self.jump(edge, len),
            ListCommand::SelectAll => {
                if len == 0 {
                    PaneResponse::Ignored
                } else {
                    self.selection.select_all(len);
                    PaneResponse::Redraw
                }
            }
            ListCommand::ClearSelection => {
                if self.selection.is_empty() && self.selection.cursor().is_none() {
                    PaneResponse::Ignored
                } else {
                    self.selection.clear();
                    PaneResponse::Redraw
                }
            }
            ListCommand::Activate => self
                .selection
                .cursor()
                .map_or(PaneResponse::Ignored, PaneResponse::Activate),
            ListCommand::Scroll(lines) => PaneResponse::redraw_if(
                self.viewport
                    .scroll_by(lines.saturating_mul(self.card_height)),
            ),
        }
    }

    /// Select a card directly and scroll its card row fully into view.
    pub fn select_item(&mut self, item: usize) -> bool {
        if item >= self.len {
            return false;
        }
        self.selection.select_only(item);
        self.viewport.scroll_into_view(item / self.columns());
        true
    }

    /// Select cards by their list indices, retaining the cursor when possible.
    ///
    /// This is the transfer seam when changing views: selection belongs to the
    /// directory, not to one particular presentation of it.
    pub fn replace_selection(&mut self, selected: &[usize], cursor: Option<usize>) {
        self.selection.clear();
        for &item in selected {
            if item < self.len {
                self.selection.toggle(item);
            }
        }
        if let Some(item) = cursor.filter(|&item| item < self.len) {
            self.selection.select_only(item);
            self.viewport.scroll_into_view(item / self.columns());
        }
    }

    /// Pointer press in pane-relative coordinates.
    pub fn press(&mut self, x: i32, y: i32, mods: Mods) -> PaneResponse {
        if self.scrollbar_visible() && x >= self.gutter_x() {
            return PaneResponse::redraw_if(self.scrollbar.press(&mut self.viewport, y));
        }
        let Some(item) = self.item_at(x, y) else {
            if self.selection.is_empty() && self.selection.cursor().is_none() {
                return PaneResponse::Ignored;
            }
            self.selection.clear();
            return PaneResponse::Redraw;
        };
        if mods.shift {
            self.selection.extend_to(item);
        } else if mods.ctrl {
            self.selection.toggle(item);
        } else {
            self.selection.select_only(item);
        }
        self.viewport.scroll_into_view(item / self.columns());
        PaneResponse::Redraw
    }

    /// Double-click selects and activates one card.
    pub fn double_click(&mut self, x: i32, y: i32) -> PaneResponse {
        if self.scrollbar_visible() && x >= self.gutter_x() {
            return PaneResponse::Ignored;
        }
        let Some(item) = self.item_at(x, y) else {
            return PaneResponse::Ignored;
        };
        self.selection.select_only(item);
        PaneResponse::Activate(item)
    }

    /// Continue a scrollbar drag.
    pub fn motion(&mut self, y: i32) -> PaneResponse {
        PaneResponse::redraw_if(self.scrollbar.drag(&mut self.viewport, y))
    }

    /// End a scrollbar drag.
    pub const fn release(&mut self) -> PaneResponse {
        let dragging = self.scrollbar.is_dragging();
        self.scrollbar.release();
        PaneResponse::redraw_if(dragging)
    }

    fn set_item_len(&mut self, len: usize) {
        self.len = len;
        let rows = len.div_ceil(self.columns());
        self.viewport.set_len(rows);
        self.selection.retain_below(len);
    }

    fn relength(&mut self) {
        self.set_item_len(self.len);
    }

    /// The card under pane-relative coordinates, excluding card gaps.
    #[must_use]
    pub fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || x >= self.content_width() {
            return None;
        }
        let row = self.viewport.row_at_y(y)?;
        let stride = self.card_width.saturating_add(self.gap).max(1);
        let column = usize::try_from(x / stride).ok()?;
        if column >= self.columns() || x % stride >= self.card_width {
            return None;
        }
        let item = row.checked_mul(self.columns())?.checked_add(column)?;
        (item < self.len).then_some(item)
    }

    fn move_cursor(&mut self, direction: Direction, len: usize, extending: bool) -> PaneResponse {
        if len == 0 {
            return PaneResponse::Ignored;
        }
        let columns = self.columns();
        let current = self.selection.cursor().unwrap_or_else(|| {
            if matches!(direction, Direction::Up) {
                len - 1
            } else {
                0
            }
        });
        let target = match direction {
            Direction::Up => current.saturating_sub(columns),
            Direction::Down => current.saturating_add(columns).min(len - 1),
        };
        let before = self.selection.cursor();
        if extending {
            self.selection.extend_to(target);
        } else {
            self.selection.select_only(target);
        }
        let scrolled = self.viewport.scroll_into_view(target / columns);
        PaneResponse::redraw_if(scrolled || before != Some(target))
    }

    fn page(&mut self, direction: Direction, len: usize) -> PaneResponse {
        let rows = (self.viewport.height() / self.card_height - 1).max(1);
        let vertical = match direction {
            Direction::Up => -rows,
            Direction::Down => rows,
        };
        let columns = i32::try_from(self.columns()).unwrap_or(i32::MAX);
        let current = i32::try_from(self.selection.cursor().unwrap_or(0)).unwrap_or(0);
        let target = current
            .saturating_add(vertical.saturating_mul(columns))
            .clamp(0, i32::try_from(len.saturating_sub(1)).unwrap_or(i32::MAX));
        self.select_item(usize::try_from(target).unwrap_or(0));
        PaneResponse::Redraw
    }

    fn jump(&mut self, edge: Edge, len: usize) -> PaneResponse {
        if len == 0 {
            return PaneResponse::Ignored;
        }
        let target = match edge {
            Edge::Top => 0,
            Edge::Bottom => len - 1,
        };
        let before = self.selection.cursor();
        self.select_item(target);
        PaneResponse::redraw_if(before != Some(target))
    }
}

#[cfg(test)]
mod tests {
    use super::GridPane;
    use crate::{Direction, ListCommand, Mods};

    impl GridPane {
        fn test_grid() -> Self {
            Self::new(350, 240, 100, 100, 10, 20)
        }
    }

    #[test]
    fn cards_fill_rows_before_wrapping() {
        let grid = GridPane::test_grid();
        assert_eq!(grid.columns(), 3);
        let cells = grid.visible_cells();
        assert_eq!(cells[0].index, 0);
        assert_eq!(cells[2].x, 220);
        assert_eq!(cells[3].y, 100);
    }

    #[test]
    fn vertical_cursor_motion_preserves_the_column() {
        let mut grid = GridPane::test_grid();
        grid.press(115, 10, Mods::none());
        grid.command(ListCommand::Move(Direction::Down));
        assert_eq!(grid.cursor(), Some(4));
    }

    #[test]
    fn a_gap_is_not_a_card_hit_target() {
        let mut grid = GridPane::test_grid();
        assert!(matches!(
            grid.press(105, 10, Mods::default()),
            crate::PaneResponse::Ignored
        ));
        assert!(grid.cursor().is_none());
    }
}
