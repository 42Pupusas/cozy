//! [`SectionState`] -- what one solid section shows, and the minimal set of
//! requests that moves it from one state to the next.

use super::logical_rect::LogicalRect;

/// A solid section's placement and its premultiplied ARGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionState {
    pub rect: LogicalRect,
    pub argb: u32,
}

/// The requests a section needs this frame. Each field is a separate request
/// on the wire, so an unchanged field costs nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionChange {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub color: Option<u32>,
    pub hide: bool,
}

impl SectionChange {
    /// Whether the section's own surface needs a commit. A position change
    /// belongs to the parent's state, so on its own it does not.
    #[must_use]
    pub const fn needs_commit(&self) -> bool {
        self.size.is_some() || self.color.is_some() || self.hide
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.position.is_none() && !self.needs_commit()
    }
}

impl SectionState {
    /// The change from `previous` (`None` for a section never shown) to
    /// `self`. A hidden section has no buffer, so showing it again must
    /// re-attach the colour and re-send the size.
    #[must_use]
    pub fn change_from(&self, previous: Option<&Self>) -> SectionChange {
        let shown_before = previous.filter(|p| p.rect.is_visible());
        if !self.rect.is_visible() {
            return SectionChange {
                hide: shown_before.is_some(),
                ..SectionChange::default()
            };
        }
        let Some(prev) = shown_before else {
            return SectionChange {
                position: Some((self.rect.x, self.rect.y)),
                size: Some((self.rect.w, self.rect.h)),
                color: Some(self.argb),
                hide: false,
            };
        };
        let moved = (prev.rect.x, prev.rect.y) != (self.rect.x, self.rect.y);
        let resized = (prev.rect.w, prev.rect.h) != (self.rect.w, self.rect.h);
        SectionChange {
            position: moved.then_some((self.rect.x, self.rect.y)),
            size: resized.then_some((self.rect.w, self.rect.h)),
            color: (prev.argb != self.argb).then_some(self.argb),
            hide: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LogicalRect, SectionChange, SectionState};

    fn state(x: i32, y: i32, w: i32, h: i32, argb: u32) -> SectionState {
        SectionState {
            rect: LogicalRect { x, y, w, h },
            argb,
        }
    }

    #[test]
    fn a_new_section_sends_everything() {
        let change = state(1, 2, 3, 4, 7).change_from(None);
        assert_eq!(
            change,
            SectionChange {
                position: Some((1, 2)),
                size: Some((3, 4)),
                color: Some(7),
                hide: false,
            }
        );
    }

    #[test]
    fn an_unchanged_section_sends_nothing() {
        let s = state(1, 2, 3, 4, 7);
        assert!(s.change_from(Some(&s)).is_empty());
    }

    #[test]
    fn a_move_alone_does_not_commit_the_child() {
        let change = state(5, 2, 3, 4, 7).change_from(Some(&state(1, 2, 3, 4, 7)));
        assert_eq!(change.position, Some((5, 2)));
        assert!(!change.needs_commit());
    }

    #[test]
    fn a_recolour_alone_sends_only_the_colour() {
        let change = state(1, 2, 3, 4, 9).change_from(Some(&state(1, 2, 3, 4, 7)));
        assert_eq!(
            change,
            SectionChange {
                color: Some(9),
                ..SectionChange::default()
            }
        );
    }

    #[test]
    fn shrinking_to_nothing_hides_once() {
        let shown = state(1, 2, 3, 4, 7);
        let gone = state(1, 2, 0, 4, 7);
        assert!(gone.change_from(Some(&shown)).hide);
        assert!(gone.change_from(Some(&gone)).is_empty());
    }

    #[test]
    fn showing_a_hidden_section_resends_everything() {
        let change = state(1, 2, 3, 4, 7).change_from(Some(&state(1, 2, 0, 0, 7)));
        assert!(change.position.is_some() && change.size.is_some() && change.color.is_some());
    }
}
