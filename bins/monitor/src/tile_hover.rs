//! [`TileHover`] -- which process or "more" tile the pointer is over.

use platform::draw::Point;

use crate::tile_layout::{Arrangement, TileKind};

#[derive(Default)]
pub struct TileHover {
    over: Option<TileKind>,
}

impl TileHover {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Follow the pointer at `at` (`None` once it leaves the window). Returns
    /// the tiles whose hover state changed: the one left and the one entered.
    pub fn track(&mut self, arrangement: &Arrangement, at: Option<Point>) -> Vec<TileKind> {
        let over = at
            .and_then(|point| arrangement.at(point))
            .map(|tile| tile.kind.clone())
            .filter(|kind| matches!(kind, TileKind::Process(_) | TileKind::More { .. }));
        if over == self.over {
            return Vec::new();
        }
        let left = std::mem::replace(&mut self.over, over.clone());
        left.into_iter().chain(over).collect()
    }

    #[must_use]
    pub fn is(&self, kind: &TileKind) -> bool {
        match (&self.over, kind) {
            (Some(TileKind::More { .. }), TileKind::More { .. }) => true,
            (Some(over), kind) => over == kind,
            (None, _) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileHover;
    use crate::tile_layout::{Arrangement, PlacedTile, TileKind};
    use platform::draw::{Point, Rect};

    fn arrangement() -> Arrangement {
        Arrangement {
            tiles: vec![
                PlacedTile {
                    kind: TileKind::Info,
                    rect: Rect::new(0, 0, 100, 50),
                },
                PlacedTile {
                    kind: TileKind::Process("helium".to_owned()),
                    rect: Rect::new(0, 50, 100, 50),
                },
                PlacedTile {
                    kind: TileKind::More { count: 3 },
                    rect: Rect::new(100, 50, 100, 50),
                },
            ],
            shown: 1,
        }
    }

    #[test]
    fn entering_and_leaving_a_tile_are_changes() {
        let mut hover = TileHover::new();
        let helium = TileKind::Process("helium".to_owned());
        assert_eq!(
            hover.track(&arrangement(), Some(Point::new(10, 60))),
            std::slice::from_ref(&helium)
        );
        assert!(hover.is(&helium));
        assert!(
            hover
                .track(&arrangement(), Some(Point::new(20, 70)))
                .is_empty()
        );
        let moved = hover.track(&arrangement(), Some(Point::new(150, 60)));
        assert_eq!(moved, [helium.clone(), TileKind::More { count: 3 }]);
        assert_eq!(
            hover.track(&arrangement(), None),
            [TileKind::More { count: 3 }]
        );
        assert!(!hover.is(&helium));
    }

    #[test]
    fn only_process_and_more_tiles_hover() {
        let mut hover = TileHover::new();
        assert!(
            hover
                .track(&arrangement(), Some(Point::new(10, 10)))
                .is_empty()
        );
        assert_eq!(
            hover.track(&arrangement(), Some(Point::new(150, 60))).len(),
            1
        );
        assert!(hover.is(&TileKind::More { count: 7 }));
    }
}
