use std::path::Path;

use browse::DirectoryBrowser;

use crate::row_format::RowFormat;
use crate::row_kind::RowKind;
use crate::size_ledger::Measure;
use crate::theme::Theme;
use crate::tile_action::TileAction;
use crate::tile_field::{PlacedMore, PlacedTile};
use crate::tile_growth::Shape;
use crate::tile_id::TileId;
use crate::tile_motion::TileKey;
use crate::tile_painter::{PaintedTile, PaintedTray};
use crate::tile_scene::TileScene;

/// What a window adds to a tile beyond the folder listing itself.
pub trait TileOverlay {
    fn is_cut(&self, path: &Path) -> bool;
    fn is_drop_target(&self, row: usize) -> bool;
    fn tray(&self, row: usize) -> Option<PaintedTray>;
}

/// An overlay that adds nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainTiles;

impl TileOverlay for PlainTiles {
    fn is_cut(&self, _: &Path) -> bool {
        false
    }

    fn is_drop_target(&self, _: usize) -> bool {
        false
    }

    fn tray(&self, _: usize) -> Option<PaintedTray> {
        None
    }
}

/// Gathers the tiles in view into the data the painter draws.
pub struct TileAssembly<'a> {
    pub scene: &'a TileScene,
    pub nav: &'a DirectoryBrowser,
    pub theme: &'a Theme,
    pub overlay: &'a dyn TileOverlay,
}

impl TileAssembly<'_> {
    /// Tiles shrinking away first, then the ones the layout holds, each drawn
    /// where its glide has got to, and the more tile last.
    #[must_use]
    pub fn tiles(&self, placed: &[PlacedTile], more: Option<PlacedMore>) -> Vec<PaintedTile> {
        let growth = self.scene.growth();
        let leaving = growth.leaving().into_iter().filter_map(|(id, row, shape)| {
            let entry = self.nav.entry(row)?;
            (TileId::of(&entry.name) == id).then(|| self.entry_tile(row, shape))?
        });
        let held = placed.iter().filter_map(|placed| {
            let entry = self.nav.entry(placed.row)?;
            let shape = growth
                .shape(TileId::of(&entry.name))
                .unwrap_or_else(|| Shape::resting(placed.rect, placed.slot));
            self.entry_tile(placed.row, shape)
        });
        let more = more.map(|more| {
            let shape = growth
                .shape(TileId::More)
                .unwrap_or_else(|| Shape::resting(more.rect, more.slot));
            self.more_tile(&more, shape)
        });
        leaving.chain(held).chain(more).collect()
    }

    fn more_tile(&self, more: &PlacedMore, shape: Shape) -> PaintedTile {
        PaintedTile {
            rect: shape.rect,
            slot: shape.slot,
            from_slot: shape.from_slot,
            mix: shape.mix,
            kind: RowKind::Leaf,
            name: format!("{} more…", more.hidden),
            detail: RowFormat::human_size(more.bytes),
            name_color: self.theme.text,
            chosen: 0,
            drop_target: false,
            raise: self.scene.motion().raise(TileKey::More),
            reveal: 0,
            grown: shape.grown,
            glow: [0; TileAction::ALL.len()],
            press: [0; TileAction::ALL.len()],
            tray: None,
        }
    }

    fn entry_tile(&self, row: usize, shape: Shape) -> Option<PaintedTile> {
        let entry = self.nav.entry(row)?;
        let key = TileKey::Item(row);
        let sizes = self.scene.field().sizes();
        let motion = self.scene.motion();
        let detail = match sizes.measure(entry) {
            Measure::Unknown => String::from("measuring…"),
            Measure::Partial(bytes) | Measure::Stale(bytes) => {
                format!("{}…", RowFormat::human_size(bytes))
            }
            Measure::Final(bytes) => RowFormat::human_size(bytes),
        };
        let name_color = if self.overlay.is_cut(&entry.path) {
            self.theme.muted
        } else {
            RowFormat::color(entry, self.theme)
        };
        Some(PaintedTile {
            rect: shape.rect,
            slot: shape.slot,
            from_slot: shape.from_slot,
            mix: shape.mix,
            kind: RowKind::of(entry),
            name: entry.name.clone(),
            detail,
            name_color,
            chosen: motion.chosen(row),
            drop_target: self.overlay.is_drop_target(row),
            raise: motion.raise(key),
            reveal: motion.reveal(key),
            grown: shape.grown,
            glow: TileAction::ALL.map(|action| motion.glow(row, action)),
            press: TileAction::ALL.map(|action| motion.press(row, action)),
            tray: self.overlay.tray(row),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{PlainTiles, TileAssembly, TileOverlay};
    use crate::row_kind::RowKind;
    use crate::test_support::TempDir;
    use crate::theme::Theme;
    use crate::tile_painter::PaintedTray;
    use crate::tile_scene::{Flow, MotionAim, TileScene};
    use browse::DirectoryBrowser;
    use platform::draw::Rect;

    struct Marking {
        cut: String,
        drop_row: usize,
    }

    impl TileOverlay for Marking {
        fn is_cut(&self, path: &Path) -> bool {
            path.file_name().is_some_and(|name| *name == *self.cut)
        }

        fn is_drop_target(&self, row: usize) -> bool {
            row == self.drop_row
        }

        fn tray(&self, _: usize) -> Option<PaintedTray> {
            None
        }
    }

    struct Fixture {
        _dir: TempDir,
        nav: DirectoryBrowser,
        scene: TileScene,
        flow: Flow,
        theme: Theme,
    }

    impl Fixture {
        fn new(name: &str, files: usize) -> Self {
            let dir = TempDir::new(name);
            for i in 0..files {
                dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
            }
            dir.create_dir("sub");
            let nav = DirectoryBrowser::open(dir.path(), 800, 600, 28).expect("open");
            let flow = Flow {
                area: Rect::new(0, 0, 1300, 800),
                floor: 120,
                tail: None,
            };
            let mut scene = TileScene::new(waker::Waker::new().expect("eventfd"));
            let aim = MotionAim {
                spot: None,
                cursor: None,
                selected: &[],
                tray: None,
                actions: false,
                held: None,
            };
            let mut now = 0;
            while scene.step(&flow, &nav, &aim, now) {
                now += 16;
                assert!(now < 20_000);
            }
            Self {
                _dir: dir,
                nav,
                scene,
                flow,
                theme: Theme::default(),
            }
        }

        fn tiles(&self, overlay: &dyn TileOverlay) -> Vec<crate::tile_painter::PaintedTile> {
            let assembly = TileAssembly {
                scene: &self.scene,
                nav: &self.nav,
                theme: &self.theme,
                overlay,
            };
            assembly.tiles(&self.scene.layout(&self.flow), self.scene.more(&self.flow))
        }
    }

    #[test]
    fn every_listed_entry_becomes_a_tile_named_after_it() {
        let fixture = Fixture::new("assembly_names", 3);
        let tiles = fixture.tiles(&PlainTiles);
        assert_eq!(tiles.len(), 4);
        assert!(tiles.iter().any(|tile| tile.name == "sub"));
        assert!(tiles.iter().any(|tile| tile.name == "f00"));
    }

    #[test]
    fn a_folder_tile_is_painted_as_a_folder() {
        let fixture = Fixture::new("assembly_kind", 1);
        let tiles = fixture.tiles(&PlainTiles);
        let sub = tiles.iter().find(|tile| tile.name == "sub").expect("sub");
        let file = tiles.iter().find(|tile| tile.name == "f00").expect("f00");
        assert_ne!(sub.kind, file.kind);
        assert_ne!(sub.name_color, file.name_color);
    }

    #[test]
    fn a_file_shows_its_size() {
        let fixture = Fixture::new("assembly_size", 1);
        let tiles = fixture.tiles(&PlainTiles);
        let file = tiles.iter().find(|tile| tile.name == "f00").expect("f00");
        assert_eq!(file.detail, "100 B");
    }

    #[test]
    fn a_cut_entry_takes_the_muted_colour() {
        let fixture = Fixture::new("assembly_cut", 2);
        let overlay = Marking {
            cut: "f00".to_owned(),
            drop_row: usize::MAX,
        };
        let tiles = fixture.tiles(&overlay);
        let cut = tiles.iter().find(|tile| tile.name == "f00").expect("f00");
        let kept = tiles.iter().find(|tile| tile.name == "f01").expect("f01");
        assert_eq!(cut.name_color, fixture.theme.muted);
        assert_ne!(kept.name_color, fixture.theme.muted);
    }

    #[test]
    fn only_the_row_the_overlay_names_is_a_drop_target() {
        let fixture = Fixture::new("assembly_drop", 2);
        let row = (0..3)
            .find(|row| {
                fixture
                    .nav
                    .entry(*row)
                    .is_some_and(|entry| entry.name == "sub")
            })
            .expect("sub row");
        let overlay = Marking {
            cut: String::new(),
            drop_row: row,
        };
        let tiles = fixture.tiles(&overlay);
        let targets: Vec<&str> = tiles
            .iter()
            .filter(|tile| tile.drop_target)
            .map(|tile| tile.name.as_str())
            .collect();
        assert_eq!(targets, ["sub"]);
    }

    #[test]
    fn plain_tiles_carry_no_buttons_and_no_tray() {
        let fixture = Fixture::new("assembly_plain", 3);
        for tile in fixture.tiles(&PlainTiles) {
            assert_eq!(tile.reveal, 0);
            assert!(tile.tray.is_none());
            assert!(tile.glow.iter().all(|level| *level == 0));
            assert!(!tile.drop_target);
        }
    }

    #[test]
    fn a_crowded_folder_ends_with_a_more_tile_that_counts_the_rest() {
        let mut fixture = Fixture::new("assembly_more", 40);
        fixture.flow.floor = 120;
        let tiles = fixture.tiles(&PlainTiles);
        let last = tiles.last().expect("tiles");
        assert!(last.name.ends_with(" more…"), "{}", last.name);
        assert_eq!(last.kind, RowKind::Leaf);
    }
}
