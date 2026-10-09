//! [`SpiralCursor`] -- where a keyboard step lands on the tiles in view, in
//! rank order rather than listing order.

use widget::{Direction, Edge, ListCommand};

/// The outcome of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// Move the cursor to this visible-entry index.
    At(usize),
    /// Scroll the field by this many tiles (positive brings in smaller
    /// ones), then put the cursor on the view's edge in that direction.
    Slide(i32),
    /// Nothing to move to.
    Stay,
}

/// The tiles in view, as the keyboard sees them.
#[derive(Debug, Clone, Copy)]
pub struct SpiralCursor<'a> {
    /// The entries in view, largest first.
    page: &'a [usize],
    /// Whether smaller entries lie past the view.
    later: bool,
    /// Whether larger entries lie before the view.
    earlier: bool,
}

impl<'a> SpiralCursor<'a> {
    #[must_use]
    pub const fn new(page: &'a [usize], later: bool, earlier: bool) -> Self {
        Self {
            page,
            later,
            earlier,
        }
    }

    /// Whether `command` is a movement this type resolves. Everything else
    /// (activate, select all, clear) means the same in every presentation.
    #[must_use]
    pub const fn steers(command: ListCommand) -> bool {
        matches!(
            command,
            ListCommand::Move(_)
                | ListCommand::Extend(_)
                | ListCommand::Jump(_)
                | ListCommand::Page(_)
        )
    }

    #[must_use]
    pub fn land(&self, cursor: Option<usize>, command: ListCommand) -> Landing {
        match command {
            ListCommand::Move(Direction::Down) | ListCommand::Extend(Direction::Down) => {
                self.next(cursor)
            }
            ListCommand::Move(Direction::Up) | ListCommand::Extend(Direction::Up) => {
                self.prev(cursor)
            }
            ListCommand::Jump(Edge::Top) => Self::edge(self.page.first()),
            ListCommand::Jump(Edge::Bottom) => Self::edge(self.page.last()),
            ListCommand::Page(Direction::Down) if self.later => Landing::Slide(self.span()),
            ListCommand::Page(Direction::Up) if self.earlier => Landing::Slide(-self.span()),
            _ => Landing::Stay,
        }
    }

    fn span(&self) -> i32 {
        i32::try_from(self.page.len().max(1)).unwrap_or(i32::MAX)
    }

    fn position(&self, cursor: Option<usize>) -> Option<usize> {
        cursor.and_then(|row| self.page.iter().position(|&r| r == row))
    }

    fn next(&self, cursor: Option<usize>) -> Landing {
        match self.position(cursor) {
            Some(i) if i + 1 < self.page.len() => Landing::At(self.page[i + 1]),
            Some(_) if self.later => Landing::Slide(1),
            Some(_) => Landing::Stay,
            None => Self::edge(self.page.first()),
        }
    }

    fn prev(&self, cursor: Option<usize>) -> Landing {
        match self.position(cursor) {
            Some(i) if i > 0 => Landing::At(self.page[i - 1]),
            Some(_) if self.earlier => Landing::Slide(-1),
            Some(_) => Landing::Stay,
            None => Self::edge(self.page.last()),
        }
    }

    fn edge(row: Option<&usize>) -> Landing {
        row.map_or(Landing::Stay, |&row| Landing::At(row))
    }
}

#[cfg(test)]
mod tests {
    use super::{Landing, SpiralCursor};
    use widget::{Direction, Edge, ListCommand};

    const DOWN: ListCommand = ListCommand::Move(Direction::Down);
    const UP: ListCommand = ListCommand::Move(Direction::Up);

    #[test]
    fn down_and_up_walk_rank_order_not_listing_order() {
        let page = [4, 0, 2];
        let cursor = SpiralCursor::new(&page, false, false);
        assert_eq!(cursor.land(Some(4), DOWN), Landing::At(0));
        assert_eq!(cursor.land(Some(0), DOWN), Landing::At(2));
        assert_eq!(cursor.land(Some(2), UP), Landing::At(0));
    }

    #[test]
    fn without_a_cursor_down_starts_at_the_largest_and_up_at_the_smallest() {
        let page = [4, 0, 2];
        let cursor = SpiralCursor::new(&page, false, false);
        assert_eq!(cursor.land(None, DOWN), Landing::At(4));
        assert_eq!(cursor.land(None, UP), Landing::At(2));
    }

    #[test]
    fn a_cursor_out_of_view_counts_as_none() {
        let page = [4, 0];
        let cursor = SpiralCursor::new(&page, false, true);
        assert_eq!(cursor.land(Some(9), DOWN), Landing::At(4));
    }

    #[test]
    fn the_ends_of_the_whole_ranking_stay_put() {
        let page = [1, 0];
        let cursor = SpiralCursor::new(&page, false, false);
        assert_eq!(cursor.land(Some(0), DOWN), Landing::Stay);
        assert_eq!(cursor.land(Some(1), UP), Landing::Stay);
    }

    #[test]
    fn walking_off_the_view_slides_it_by_one_tile() {
        let page = [1, 0];
        let cursor = SpiralCursor::new(&page, true, true);
        assert_eq!(cursor.land(Some(0), DOWN), Landing::Slide(1));
        assert_eq!(cursor.land(Some(1), UP), Landing::Slide(-1));
        assert_eq!(
            cursor.land(Some(0), ListCommand::Extend(Direction::Down)),
            Landing::Slide(1)
        );
    }

    #[test]
    fn home_and_end_jump_to_the_ends_of_the_view() {
        let page = [3, 1, 2];
        let cursor = SpiralCursor::new(&page, true, false);
        assert_eq!(
            cursor.land(Some(1), ListCommand::Jump(Edge::Top)),
            Landing::At(3)
        );
        assert_eq!(
            cursor.land(None, ListCommand::Jump(Edge::Bottom)),
            Landing::At(2)
        );
    }

    #[test]
    fn page_keys_slide_a_view_length_only_where_there_is_more() {
        let page = [0, 1, 2];
        let flat = SpiralCursor::new(&page, false, false);
        assert_eq!(
            flat.land(None, ListCommand::Page(Direction::Down)),
            Landing::Stay
        );
        assert_eq!(
            flat.land(None, ListCommand::Page(Direction::Up)),
            Landing::Stay
        );
        let deep = SpiralCursor::new(&page, true, true);
        assert_eq!(
            deep.land(None, ListCommand::Page(Direction::Down)),
            Landing::Slide(3)
        );
        assert_eq!(
            deep.land(None, ListCommand::Page(Direction::Up)),
            Landing::Slide(-3)
        );
    }

    #[test]
    fn an_empty_view_never_moves() {
        let cursor = SpiralCursor::new(&[], false, false);
        assert_eq!(cursor.land(None, DOWN), Landing::Stay);
        assert_eq!(
            cursor.land(None, ListCommand::Jump(Edge::Top)),
            Landing::Stay
        );
    }

    #[test]
    fn only_movement_is_steered() {
        assert!(SpiralCursor::steers(DOWN));
        assert!(SpiralCursor::steers(ListCommand::Page(Direction::Up)));
        assert!(!SpiralCursor::steers(ListCommand::Activate));
        assert!(!SpiralCursor::steers(ListCommand::SelectAll));
        assert!(!SpiralCursor::steers(ListCommand::Scroll(1)));
    }
}
