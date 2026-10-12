//! [`TileStage`] -- remembers the folder last drawn, so that when another
//! folder replaces it the zoom between them can be played with the old
//! tiles still in hand.

use std::path::{Path, PathBuf};

use platform::draw::Rect;

use crate::tile_painter::PaintedTile;
use crate::tile_passage::Passage;
use crate::tile_transition::TileTransition;

struct Seen {
    folder: PathBuf,
    tiles: Vec<PaintedTile>,
}

#[derive(Default)]
pub struct TileStage {
    seen: Option<Seen>,
    transition: TileTransition,
}

impl TileStage {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seen: None,
            transition: TileTransition::new(),
        }
    }

    /// Note that `folder` is the one shown in `field`. When it replaces the
    /// folder last drawn, start the zoom between them and return whether
    /// the new folder's tiles should arrive already grown.
    pub fn notice(&mut self, folder: &Path, field: Rect, now_ms: u64) -> bool {
        self.transition.keep_within(field);
        let Some(seen) = self.seen.as_mut() else {
            return false;
        };
        if seen.folder == folder {
            return false;
        }
        let passage = Passage::between(Some(&seen.folder), folder);
        let ghosts = std::mem::take(&mut seen.tiles);
        seen.folder = folder.to_path_buf();
        self.transition.start(passage, field, ghosts, now_ms);
        self.transition.arrives_grown()
    }

    /// The tiles of `folder`, `shown`, as this frame draws them, together
    /// with the old folder's tiles still in sight. `shown` is remembered for
    /// the next change of folder.
    pub fn stage(
        &mut self,
        folder: &Path,
        field: Rect,
        shown: Vec<PaintedTile>,
        now_ms: u64,
    ) -> Vec<PaintedTile> {
        self.notice(folder, field, now_ms);
        let staged = self.transition.stage(shown.clone());
        self.seen = Some(Seen {
            folder: folder.to_path_buf(),
            tiles: shown.into_iter().map(PaintedTile::at_rest).collect(),
        });
        staged
    }

    pub fn advance(&mut self, now_ms: u64) {
        self.transition.advance(now_ms);
    }

    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.transition.is_animating()
    }
}

#[cfg(test)]
mod tests {
    use super::TileStage;
    use crate::row_kind::RowKind;
    use crate::tile_action::TileAction;
    use crate::tile_id::TileId;
    use crate::tile_painter::PaintedTile;
    use platform::draw::{Color, Rect};
    use std::path::Path;

    const FIELD: Rect = Rect::new(0, 0, 1000, 600);

    fn tile(name: &str, rect: Rect) -> PaintedTile {
        PaintedTile {
            id: TileId::of(name),
            rect,
            slot: 0,
            from_slot: 0,
            mix: u8::MAX,
            kind: RowKind::Navigable,
            name: name.to_owned(),
            detail: String::new(),
            name_color: Color::WHITE,
            chosen: 0,
            drop_target: false,
            raise: 200,
            reveal: 0,
            grown: u8::MAX,
            veil: 0,
            layer: 0,
            glow: [0; TileAction::ALL.len()],
            press: [0; TileAction::ALL.len()],
            tray: None,
        }
    }

    fn parent() -> Vec<PaintedTile> {
        vec![
            tile("big", Rect::new(0, 0, 600, 600)),
            tile("dir", Rect::new(600, 0, 400, 600)),
        ]
    }

    #[test]
    fn nothing_zooms_before_a_folder_has_been_drawn() {
        let mut stage = TileStage::new();
        assert!(!stage.notice(Path::new("/a"), FIELD, 0));
        assert!(!stage.is_animating());
    }

    #[test]
    fn drawing_the_same_folder_again_does_not_zoom() {
        let mut stage = TileStage::new();
        stage.stage(Path::new("/a"), FIELD, parent(), 0);
        let again = stage.stage(Path::new("/a"), FIELD, parent(), 16);
        assert_eq!(again, parent());
        assert!(!stage.is_animating());
    }

    #[test]
    fn opening_a_drawn_folder_zooms_into_it_with_the_old_tiles_at_rest() {
        let mut stage = TileStage::new();
        stage.stage(Path::new("/a"), FIELD, parent(), 0);
        assert!(!stage.notice(Path::new("/a/dir"), FIELD, 16));
        assert!(stage.is_animating());
        let staged = stage.stage(Path::new("/a/dir"), FIELD, Vec::new(), 16);
        assert_eq!(staged.len(), 2);
        assert!(staged.iter().all(|ghost| ghost.raise == 0));
    }

    #[test]
    fn going_up_asks_for_the_parent_to_arrive_grown() {
        let mut stage = TileStage::new();
        stage.stage(Path::new("/a/dir"), FIELD, parent(), 0);
        assert!(stage.notice(Path::new("/a"), FIELD, 16));
        assert!(stage.is_animating());
    }

    #[test]
    fn the_zoom_plays_out_and_stops() {
        let mut stage = TileStage::new();
        stage.stage(Path::new("/a"), FIELD, parent(), 0);
        stage.notice(Path::new("/a/dir"), FIELD, 0);
        for frame in 1..=40 {
            stage.advance(frame * 16);
        }
        assert!(!stage.is_animating());
    }
}
