use portal_core::Selection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooserStep {
    Ignored,
    Redraw,
    Moved,
    Finished(Selection),
    Cancelled,
}

impl ChooserStep {
    #[must_use]
    pub const fn redraw_if(changed: bool) -> Self {
        if changed { Self::Redraw } else { Self::Ignored }
    }

    #[must_use]
    pub const fn needs_redraw(&self) -> bool {
        matches!(self, Self::Redraw | Self::Moved)
    }

    #[must_use]
    pub const fn ends_dialog(&self) -> bool {
        matches!(self, Self::Finished(_) | Self::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redraw_if_maps_a_change_to_a_redraw() {
        assert_eq!(ChooserStep::redraw_if(true), ChooserStep::Redraw);
        assert_eq!(ChooserStep::redraw_if(false), ChooserStep::Ignored);
    }

    #[test]
    fn only_finishing_and_cancelling_end_the_dialog() {
        assert!(ChooserStep::Cancelled.ends_dialog());
        assert!(ChooserStep::Finished(Selection::default()).ends_dialog());
        assert!(!ChooserStep::Moved.ends_dialog());
        assert!(!ChooserStep::Redraw.ends_dialog());
        assert!(!ChooserStep::Ignored.ends_dialog());
    }

    #[test]
    fn moving_and_redrawing_owe_a_repaint() {
        assert!(ChooserStep::Moved.needs_redraw());
        assert!(ChooserStep::Redraw.needs_redraw());
        assert!(!ChooserStep::Ignored.needs_redraw());
        assert!(!ChooserStep::Cancelled.needs_redraw());
    }
}
