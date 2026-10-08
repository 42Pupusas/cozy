//! [`Frame`] -- the window's top-level geometry: the field the spiral fills,
//! and what the pinned chrome card needs from it.
//!
//! Pure arithmetic over a window size, so the whole layout is testable
//! without a compositor. The card has no rectangle of its own here: it is the
//! spiral's innermost cell, so its position follows from how many tiles the
//! directory has. The frame supplies the field and the card's minimum size;
//! [`Frame::card_in`] fits the card to whichever cell the spiral hands back.

use platform::draw::Rect;

use crate::pinned_card::{CardMetrics, PinnedCard};
use crate::spacing::Spacing;
use crate::spiral::MinCell;

/// The window's resolved layout.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    size: Rect,
    field: Rect,
    need: Option<MinCell>,
    spacing: Spacing,
    metrics: CardMetrics,
}

impl Frame {
    /// Resolve the layout for a window of `width` x `height`.
    #[must_use]
    pub fn new(
        width: i32,
        height: i32,
        card_visible: bool,
        spacing: Spacing,
        metrics: CardMetrics,
    ) -> Self {
        let width = width.max(0);
        let height = height.max(0);
        let margin = spacing.gutter();
        let field = Rect::new(
            margin,
            margin,
            (width - margin * 2).max(0),
            (height - margin * 2).max(0),
        );
        let gutters = spacing.gutter() * 2;
        let need = card_visible.then(|| MinCell {
            w: spacing.tile_min() + gutters,
            h: metrics.nav
                + metrics.crumbs
                + metrics.filter
                + metrics.status
                + metrics.places_row
                + gutters,
        });
        Self {
            size: Rect::new(0, 0, width, height),
            field,
            need,
            spacing,
            metrics,
        }
    }

    /// The whole window, for modals that float over everything.
    #[must_use]
    pub const fn size(&self) -> Rect {
        self.size
    }

    /// Everything inside the margins: the area the spiral fills, card
    /// included.
    #[must_use]
    pub const fn field(&self) -> Rect {
        self.field
    }

    /// The least room the card needs, or `None` when the user hid it.
    #[must_use]
    pub const fn card_need(&self) -> Option<MinCell> {
        self.need
    }

    /// The card fitted to the spiral cell `cell`, inset by the same gutter
    /// the tiles use so it sits in its slot exactly as a tile does.
    #[must_use]
    pub fn card_in(&self, cell: Rect) -> PinnedCard {
        let gutter = self.spacing.gutter();
        let inner = Rect::new(
            cell.x + gutter,
            cell.y + gutter,
            (cell.w - gutter * 2).max(0),
            (cell.h - gutter * 2).max(0),
        );
        PinnedCard::new(inner, self.spacing, self.metrics)
    }
}

#[cfg(test)]
mod tests {
    use super::Frame;
    use crate::pinned_card::CardMetrics;
    use crate::spacing::Spacing;
    use platform::draw::Rect;

    const METRICS: CardMetrics = CardMetrics {
        nav: 58,
        crumbs: 34,
        filter: 43,
        places_row: 34,
        storage: 42,
        status: 35,
    };

    fn spacing() -> Spacing {
        Spacing::new(13.0, 1.0)
    }

    fn frame(width: i32, height: i32) -> Frame {
        Frame::new(width, height, true, spacing(), METRICS)
    }

    #[test]
    fn the_field_is_the_inner_window() {
        let g = spacing().gutter();
        assert_eq!(
            frame(1280, 790).field(),
            Rect::new(g, g, 1280 - g * 2, 790 - g * 2)
        );
    }

    #[test]
    fn the_window_margin_equals_the_gap_between_cells() {
        let g = spacing().gutter();
        let frame = frame(1280, 790);
        let drawn = frame.card_in(frame.field()).bounds();
        let between_neighbours = g * 2;
        assert_eq!(drawn.x, between_neighbours);
        assert_eq!(drawn.y, between_neighbours);
    }

    #[test]
    fn hiding_the_card_keeps_the_field_and_drops_the_need() {
        let hidden = Frame::new(1280, 790, false, spacing(), METRICS);
        assert!(hidden.card_need().is_none());
        assert_eq!(hidden.field(), frame(1280, 790).field());
    }

    #[test]
    fn the_card_needs_one_tile_width_and_its_chrome_plus_a_place() {
        let need = frame(1280, 790).card_need().expect("shown");
        let gutters = spacing().gutter() * 2;
        assert_eq!(need.w, spacing().tile_min() + gutters);
        assert_eq!(need.h, 58 + 34 + 43 + 35 + 34 + gutters);
    }

    #[test]
    fn the_card_fits_whatever_cell_it_is_given() {
        let cell = Rect::new(100, 200, 160, 420);
        let g = spacing().gutter();
        let card = frame(1280, 790).card_in(cell);
        assert_eq!(
            card.bounds(),
            Rect::new(100 + g, 200 + g, 160 - g * 2, 420 - g * 2)
        );
    }

    #[test]
    fn a_tiny_window_yields_no_negative_geometry() {
        for height in [0, 1, 20, 60, 90, 300] {
            for width in [0, 1, 40, 90, 300] {
                let f = frame(width, height);
                let field = f.field();
                assert!(field.w >= 0 && field.h >= 0, "{width}x{height}: {field:?}");
            }
        }
    }
}
