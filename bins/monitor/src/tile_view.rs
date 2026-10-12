//! [`TileView`] -- what the process tiles show: the top-level groups or one
//! group's members, and how many of the largest the field has slid past.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TileView {
    group: Option<String>,
    offset: usize,
    root_offset: usize,
}

impl TileView {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }

    /// Rank of the first entry in view.
    #[must_use]
    pub const fn start(&self) -> usize {
        self.offset
    }

    /// Slide `steps` entries: positive drops the largest from view, negative
    /// brings them back. `later` says whether entries lie past the view.
    /// Returns whether the view moved.
    pub fn slide(&mut self, steps: i32, later: bool) -> bool {
        let before = self.offset;
        let by = usize::try_from(steps.unsigned_abs()).unwrap_or(usize::MAX);
        self.offset = if steps > 0 {
            if later { self.offset + by } else { self.offset }
        } else {
            self.offset.saturating_sub(by)
        };
        self.offset != before
    }

    /// Show the members of `group`, remembering where the groups were.
    pub fn expand(&mut self, group: &str) -> bool {
        if self.group.is_some() {
            return false;
        }
        self.root_offset = std::mem::take(&mut self.offset);
        self.group = Some(group.to_owned());
        true
    }

    /// Slide back to the largest entries, or else leave the group. Returns
    /// whether anything changed.
    pub fn back(&mut self) -> bool {
        if self.offset > 0 {
            self.offset = 0;
            return true;
        }
        self.leave_group()
    }

    /// Return to the groups, where they were left.
    pub fn leave_group(&mut self) -> bool {
        let left = self.group.take().is_some();
        if left {
            self.offset = std::mem::take(&mut self.root_offset);
        }
        left
    }

    /// Keep the first entry in view inside the `total` there are now.
    pub fn clamp(&mut self, total: usize) -> bool {
        let limit = total.saturating_sub(1);
        let changed = self.offset > limit;
        self.offset = self.offset.min(limit);
        changed
    }

    /// Where the view is, for the window title; `None` at the top of the
    /// groups.
    #[must_use]
    pub fn heading(&self) -> Option<String> {
        let from = self.offset + 1;
        match (self.group(), self.offset) {
            (None, 0) => None,
            (None, _) => Some(format!("from #{from}")),
            (Some(group), 0) => Some(group.to_owned()),
            (Some(group), _) => Some(format!("{group} · from #{from}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileView;

    #[test]
    fn sliding_moves_one_entry_at_a_time_and_stops_at_either_end() {
        let mut view = TileView::new();
        assert!(!view.slide(-1, true));
        assert!(view.slide(1, true));
        assert_eq!(view.start(), 1);
        assert!(!view.slide(1, false), "nothing lies past the view");
        assert!(view.slide(3, true));
        assert_eq!(view.start(), 4);
        assert!(view.slide(-10, true));
        assert_eq!(view.start(), 0);
    }

    #[test]
    fn back_returns_to_the_top_then_leaves_the_group_then_stops() {
        let mut view = TileView::new();
        view.slide(5, true);
        assert!(view.expand("helium"));
        assert_eq!(view.start(), 0);
        view.slide(2, true);
        assert!(view.back());
        assert_eq!((view.group(), view.start()), (Some("helium"), 0));
        assert!(view.back());
        assert_eq!((view.group(), view.start()), (None, 5));
        assert!(view.back());
        assert!(!view.back());
    }

    #[test]
    fn a_group_cannot_be_expanded_from_inside_another() {
        let mut view = TileView::new();
        assert!(view.expand("a"));
        assert!(!view.expand("b"));
    }

    #[test]
    fn the_view_stays_inside_a_shrinking_list() {
        let mut view = TileView::new();
        view.slide(9, true);
        assert!(!view.clamp(20));
        assert!(view.clamp(6));
        assert_eq!(view.start(), 5);
        assert!(view.clamp(0));
        assert_eq!(view.start(), 0);
    }

    #[test]
    fn the_heading_names_the_group_and_position() {
        let mut view = TileView::new();
        assert_eq!(view.heading(), None);
        view.slide(4, true);
        assert_eq!(view.heading().as_deref(), Some("from #5"));
        view.expand("helium");
        assert_eq!(view.heading().as_deref(), Some("helium"));
        view.slide(2, true);
        assert_eq!(view.heading().as_deref(), Some("helium · from #3"));
    }
}
