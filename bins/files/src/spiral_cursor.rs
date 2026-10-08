//! [`SpiralCursor`] -- where a keyboard step lands on a page of tiles, in
//! rank order rather than listing order.

use widget::{Direction, Edge, ListCommand};

/// The outcome of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// Move the cursor to this visible-entry index.
    At(usize),
    /// Walk past the page's last entry, into the overflow page.
    NextPage,
    /// Walk back before the page's first entry, out of this overflow page.
    PrevPage,
    /// Nothing to move to.
    Stay,
}

/// One page of tiles, as the keyboard sees it.
#[derive(Debug, Clone, Copy)]
pub struct SpiralCursor<'a> {
    /// The page's entries, largest first.
    page: &'a [usize],
    /// Whether the page ends in an overflow tile.
    has_more: bool,
    /// Whether this page is itself an overflow page.
    nested: bool,
}

impl<'a> SpiralCursor<'a> {
    #[must_use]
    pub const fn new(page: &'a [usize], has_more: bool, nested: bool) -> Self {
        Self {
            page,
            has_more,
            nested,
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
            ListCommand::Page(Direction::Down) if self.has_more => Landing::NextPage,
            ListCommand::Page(Direction::Up) if self.nested => Landing::PrevPage,
            _ => Landing::Stay,
        }
    }

    fn position(&self, cursor: Option<usize>) -> Option<usize> {
        cursor.and_then(|row| self.page.iter().position(|&r| r == row))
    }

    fn next(&self, cursor: Option<usize>) -> Landing {
        match self.position(cursor) {
            Some(i) if i + 1 < self.page.len() => Landing::At(self.page[i + 1]),
            Some(_) if self.has_more => Landing::NextPage,
            Some(_) => Landing::Stay,
            None => Self::edge(self.page.first()),
        }
    }

    fn prev(&self, cursor: Option<usize>) -> Landing {
        match self.position(cursor) {
            Some(i) if i > 0 => Landing::At(self.page[i - 1]),
            Some(_) if self.nested => Landing::PrevPage,
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
    fn a_cursor_left_on_another_page_counts_as_none() {
        let page = [4, 0];
        let cursor = SpiralCursor::new(&page, false, true);
        assert_eq!(cursor.land(Some(9), DOWN), Landing::At(4));
    }

    #[test]
    fn the_ends_of_a_single_page_stay_put() {
        let page = [1, 0];
        let cursor = SpiralCursor::new(&page, false, false);
        assert_eq!(cursor.land(Some(0), DOWN), Landing::Stay);
        assert_eq!(cursor.land(Some(1), UP), Landing::Stay);
    }

    #[test]
    fn walking_off_a_page_crosses_into_the_neighbouring_one() {
        let page = [1, 0];
        let cursor = SpiralCursor::new(&page, true, true);
        assert_eq!(cursor.land(Some(0), DOWN), Landing::NextPage);
        assert_eq!(cursor.land(Some(1), UP), Landing::PrevPage);
        assert_eq!(
            cursor.land(Some(0), ListCommand::Extend(Direction::Down)),
            Landing::NextPage
        );
    }

    #[test]
    fn home_and_end_jump_to_the_page_ends() {
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
    fn page_keys_open_and_close_overflow_pages_only_when_they_exist() {
        let page = [0];
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
            Landing::NextPage
        );
        assert_eq!(
            deep.land(None, ListCommand::Page(Direction::Up)),
            Landing::PrevPage
        );
    }

    #[test]
    fn an_empty_page_never_moves() {
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
