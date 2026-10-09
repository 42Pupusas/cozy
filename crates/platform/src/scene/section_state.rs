//! [`SectionState`] -- what one section shows, and the minimal set of
//! requests that moves it from one state to the next.

use super::logical_rect::LogicalRect;
use super::section_fill::SectionFill;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionState {
    pub rect: LogicalRect,
    pub fill: SectionFill,
}

/// The requests a section needs this frame. Each field is a separate request
/// on the wire, so an unchanged field costs nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SectionChange {
    pub position: Option<(i32, i32)>,
    pub size: Option<(i32, i32)>,
    pub fill: Option<SectionFill>,
    pub transform: Option<i32>,
    pub hide: bool,
}

impl SectionChange {
    /// Whether the section's own surface needs a commit. A position change
    /// belongs to the parent's state, so on its own it does not.
    #[must_use]
    pub const fn needs_commit(&self) -> bool {
        self.size.is_some() || self.fill.is_some() || self.transform.is_some() || self.hide
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.position.is_none() && !self.needs_commit()
    }
}

impl SectionState {
    /// The change from `previous` (`None` for a section never shown) to
    /// `self`. A hidden section has no buffer, so showing it again must
    /// re-attach the fill and re-send the size. The transform survives
    /// hiding, so it is compared with whatever was last sent.
    #[must_use]
    pub fn change_from(&self, previous: Option<&Self>) -> SectionChange {
        let shown_before = previous.filter(|p| p.rect.is_visible());
        if !self.rect.is_visible() {
            return SectionChange {
                hide: shown_before.is_some(),
                ..SectionChange::default()
            };
        }
        let last_transform = previous.map_or(0, |p| p.fill.transform());
        let transform = (self.fill.transform() != last_transform).then_some(self.fill.transform());
        let Some(prev) = shown_before else {
            return SectionChange {
                position: Some((self.rect.x, self.rect.y)),
                size: Some((self.rect.w, self.rect.h)),
                fill: Some(self.fill),
                transform,
                hide: false,
            };
        };
        let moved = (prev.rect.x, prev.rect.y) != (self.rect.x, self.rect.y);
        let resized = (prev.rect.w, prev.rect.h) != (self.rect.w, self.rect.h);
        SectionChange {
            position: moved.then_some((self.rect.x, self.rect.y)),
            size: resized.then_some((self.rect.w, self.rect.h)),
            fill: (prev.fill != self.fill).then_some(self.fill),
            transform,
            hide: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LogicalRect, SectionChange, SectionFill, SectionState};
    use crate::scene::quarter::Quarter;

    fn state(x: i32, y: i32, w: i32, h: i32, argb: u32) -> SectionState {
        SectionState {
            rect: LogicalRect { x, y, w, h },
            fill: SectionFill::Solid(argb),
        }
    }

    fn corner(quarter: Quarter) -> SectionState {
        SectionState {
            rect: LogicalRect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
            fill: SectionFill::Corner {
                argb: 7,
                side: 8,
                quarter,
            },
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
                fill: Some(SectionFill::Solid(7)),
                transform: None,
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
    fn a_recolour_alone_sends_only_the_fill() {
        let change = state(1, 2, 3, 4, 9).change_from(Some(&state(1, 2, 3, 4, 7)));
        assert_eq!(
            change,
            SectionChange {
                fill: Some(SectionFill::Solid(9)),
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
        assert!(change.position.is_some() && change.size.is_some() && change.fill.is_some());
    }

    #[test]
    fn a_new_corner_other_than_top_left_sends_its_turn() {
        assert_eq!(corner(Quarter::TopLeft).change_from(None).transform, None);
        assert_eq!(
            corner(Quarter::BottomLeft).change_from(None).transform,
            Some(3)
        );
    }

    #[test]
    fn turning_a_corner_sends_the_transform_and_the_fill() {
        let change = corner(Quarter::TopRight).change_from(Some(&corner(Quarter::TopLeft)));
        assert_eq!(change.transform, Some(1));
        assert!(change.fill.is_some());
    }

    #[test]
    fn a_corner_slot_reused_for_a_solid_turns_back() {
        let change = state(0, 0, 8, 8, 7).change_from(Some(&corner(Quarter::BottomRight)));
        assert_eq!(change.transform, Some(0));
    }
}
