//! [`TileGeometry`] -- where everything on one tile sits.
//!
//! The painter draws from this and the pointer handler hit-tests against it,
//! so a button can be pressed exactly where it is drawn.

use platform::draw::Rect;

use crate::spacing::Spacing;
use crate::tile_action::ActionBar;

/// The line heights of the type tiers a tile draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineHeights {
    pub title: i32,
    pub body: i32,
    pub detail: i32,
}

/// One tile's face, its text rows and its action buttons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileGeometry {
    pub face: Rect,
    pub roomy: bool,
    pub name_y: i32,
    pub detail_y: Option<i32>,
    pub bar: ActionBar,
}

impl TileGeometry {
    #[must_use]
    pub fn of(cell: Rect, spacing: Spacing, lines: LineHeights) -> Self {
        let face = spacing.tile_face(cell);
        let pad = spacing.m();
        let roomy = spacing.is_roomy_tile(face.h);
        let name_height = if roomy { lines.title } else { lines.body };
        let name_y = face.y + pad;
        let below = name_y + name_height;
        let detail_y = (below + lines.detail <= face.bottom() - pad / 2).then_some(below);
        let text_bottom = detail_y.map_or(below, |y| y + lines.detail);
        Self {
            face,
            roomy,
            name_y,
            detail_y,
            bar: ActionBar::fit(face, text_bottom, spacing),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LineHeights, TileGeometry};
    use crate::spacing::Spacing;
    use platform::draw::Rect;

    const LINES: LineHeights = LineHeights {
        title: 22,
        body: 18,
        detail: 15,
    };

    fn spacing() -> Spacing {
        Spacing::new(13.0, 1.0)
    }

    #[test]
    fn the_face_is_the_cell_less_the_gutter() {
        let cell = Rect::new(0, 0, 200, 150);
        let geometry = TileGeometry::of(cell, spacing(), LINES);
        assert_eq!(geometry.face, spacing().tile_face(cell));
    }

    #[test]
    fn a_tall_tile_sets_its_name_in_the_title_tier_and_a_short_one_in_body() {
        let tall = TileGeometry::of(Rect::new(0, 0, 200, 400), spacing(), LINES);
        let short = TileGeometry::of(Rect::new(0, 0, 200, 120), spacing(), LINES);
        assert!(tall.roomy && !short.roomy);
        let tall_detail = tall.detail_y.expect("room for detail") - tall.name_y;
        let short_detail = short.detail_y.expect("room for detail") - short.name_y;
        assert_eq!(tall_detail, LINES.title);
        assert_eq!(short_detail, LINES.body);
    }

    #[test]
    fn a_tile_with_no_room_below_the_name_drops_its_detail() {
        let flat = TileGeometry::of(Rect::new(0, 0, 200, 40), spacing(), LINES);
        assert_eq!(flat.detail_y, None);
    }

    #[test]
    fn the_buttons_sit_below_the_text() {
        let geometry = TileGeometry::of(Rect::new(0, 0, 220, 170), spacing(), LINES);
        let text_bottom = geometry.detail_y.expect("detail") + LINES.detail;
        assert!(!geometry.bar.buttons().is_empty());
        for (_, rect) in geometry.bar.buttons() {
            assert!(rect.y > text_bottom);
            assert!(rect.bottom() <= geometry.face.bottom());
        }
    }

    #[test]
    fn a_narrow_tile_has_no_buttons() {
        let geometry = TileGeometry::of(Rect::new(0, 0, 80, 170), spacing(), LINES);
        assert!(geometry.bar.buttons().is_empty());
    }
}
