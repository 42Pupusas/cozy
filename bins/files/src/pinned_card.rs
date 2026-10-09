//! [`PinnedCard`] -- the vertical sections of the always-present corner card
//! that holds every piece of chrome: navigation, path, filter, and the
//! disk-usage footer.

use platform::draw::Rect;

use crate::spacing::Spacing;

/// Heights requested by each section of the card, before fitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardMetrics {
    pub nav: i32,
    pub crumbs: i32,
    pub filter: i32,
    pub storage: i32,
}

/// The card's sections, top to bottom: nav, crumbs, filter, storage.
///
/// A window too short for every section drops them in priority order: nav,
/// crumbs, filter, then storage, which is kept only whole.
#[derive(Debug, Clone, Copy)]
pub struct PinnedCard {
    bounds: Rect,
    pad: i32,
    nav: i32,
    crumbs: i32,
    filter: i32,
    storage: i32,
}

impl PinnedCard {
    #[must_use]
    pub fn new(bounds: Rect, spacing: Spacing, metrics: CardMetrics) -> Self {
        let bounds = Rect::new(bounds.x, bounds.y, bounds.w.max(0), bounds.h.max(0));
        let mut left = bounds.h;
        let nav = Self::take(&mut left, metrics.nav);
        let crumbs = Self::take(&mut left, metrics.crumbs);
        let filter = Self::take(&mut left, metrics.filter);
        let storage = if left >= metrics.storage {
            Self::take(&mut left, metrics.storage)
        } else {
            0
        };
        Self {
            bounds,
            pad: spacing.sidebar_pad(),
            nav,
            crumbs,
            filter,
            storage,
        }
    }

    fn take(left: &mut i32, want: i32) -> i32 {
        let got = want.clamp(0, (*left).max(0));
        *left -= got;
        got
    }

    #[must_use]
    pub const fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Horizontal padding between the card's edge and its content.
    #[must_use]
    pub const fn pad(&self) -> i32 {
        self.pad
    }

    /// The row of navigation buttons and view controls.
    #[must_use]
    pub const fn nav(&self) -> Rect {
        Rect::new(self.bounds.x, self.bounds.y, self.bounds.w, self.nav)
    }

    /// The breadcrumb line, inset by the card's padding.
    #[must_use]
    pub const fn crumbs(&self) -> Rect {
        let w = self.bounds.w - self.pad * 2;
        Rect::new(
            self.bounds.x + self.pad,
            self.bounds.y + self.nav,
            if w > 0 { w } else { 0 },
            self.crumbs,
        )
    }

    /// The row the filter box sits in.
    #[must_use]
    pub const fn filter_row(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + self.nav + self.crumbs,
            self.bounds.w,
            self.filter,
        )
    }

    /// The disk-usage footer, or `None` when it was dropped for space.
    #[must_use]
    pub const fn storage(&self) -> Option<Rect> {
        if self.storage == 0 {
            return None;
        }
        Some(Rect::new(
            self.bounds.x,
            self.bounds.bottom() - self.storage,
            self.bounds.w,
            self.storage,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{CardMetrics, PinnedCard};
    use crate::spacing::Spacing;
    use platform::draw::Rect;

    const METRICS: CardMetrics = CardMetrics {
        nav: 58,
        crumbs: 34,
        filter: 43,
        storage: 42,
    };

    fn card(height: i32) -> PinnedCard {
        PinnedCard::new(
            Rect::new(12, 12, 248, height),
            Spacing::new(13.0, 1.0),
            METRICS,
        )
    }

    fn sections(card: &PinnedCard) -> Vec<Rect> {
        let mut out = vec![card.nav(), card.crumbs(), card.filter_row()];
        out.extend(card.storage());
        out
    }

    #[test]
    fn the_sections_stack_top_to_bottom_without_overlap() {
        let c = card(760);
        let stacked = sections(&c);
        for pair in stacked.windows(2) {
            assert!(
                pair[0].bottom() <= pair[1].y,
                "{:?} overlaps {:?}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(c.nav().y, c.bounds().y);
        assert_eq!(
            c.storage().map(|s| s.bottom()),
            Some(c.bounds().bottom()),
            "the footer is the card's last section"
        );
    }

    #[test]
    fn every_section_stays_inside_the_card() {
        let c = card(760);
        for s in sections(&c) {
            assert!(
                s.x >= c.bounds().x && s.right() <= c.bounds().right(),
                "{s:?}"
            );
            assert!(
                s.y >= c.bounds().y && s.bottom() <= c.bounds().bottom(),
                "{s:?}"
            );
        }
    }

    #[test]
    fn storage_is_dropped_when_it_cannot_fit_whole() {
        let tight = METRICS.nav + METRICS.crumbs + METRICS.filter + METRICS.storage - 1;
        assert!(card(tight).storage().is_none());
        assert!(card(tight + 1).storage().is_some());
    }

    #[test]
    fn a_short_card_keeps_navigation_first() {
        let c = card(METRICS.nav + 10);
        assert_eq!(c.nav().h, METRICS.nav);
        assert_eq!(c.crumbs().h, 10);
        assert_eq!(c.filter_row().h, 0);
    }

    #[test]
    fn a_degenerate_card_yields_no_negative_geometry() {
        for h in [-5, 0, 1, 30, 100] {
            for w in [-5, 0, 1, 20] {
                let c = PinnedCard::new(Rect::new(0, 0, w, h), Spacing::new(13.0, 1.0), METRICS);
                for s in sections(&c) {
                    assert!(s.w >= 0 && s.h >= 0, "{w}x{h} produced {s:?}");
                }
            }
        }
    }

    #[test]
    fn the_crumbs_are_inset_by_the_card_padding() {
        let c = card(760);
        assert_eq!(c.crumbs().x, c.bounds().x + c.pad());
        assert_eq!(c.crumbs().right(), c.bounds().right() - c.pad());
    }
}
