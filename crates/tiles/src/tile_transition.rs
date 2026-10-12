//! [`TileTransition`] -- the zoom between a folder and one inside it.
//!
//! Opening a folder expands its tile to fill the field while it fades into
//! the background, pushing its neighbours out, and the new folder's tiles
//! grow from inside it. Going up runs the same zoom backwards: the old
//! tiles shrink away in spiral order into the tile they belonged to, that
//! tile contracts to its cell and regains its fill, and its siblings come
//! back in from the edges.

use platform::draw::Rect;

use crate::fade::Fade;
use crate::tile_id::TileId;
use crate::tile_painter::PaintedTile;
use crate::tile_passage::Passage;
use crate::tile_zoom::Zoom;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Into { portal: TileId, cell: Rect },
    OutOf { portal: TileId },
}

struct Journey {
    direction: Direction,
    field: Rect,
    ghosts: Vec<PaintedTile>,
    travel: Fade,
}

impl Journey {
    const TRAVEL_MS: u16 = 420;
    const SHRINK_SPREAD: f32 = 0.35;
    const SHRINK_SPAN: f32 = 0.45;

    fn stage(&self, shown: Vec<PaintedTile>) -> Vec<PaintedTile> {
        let level = self.travel.level();
        match self.direction {
            Direction::Into { portal, cell } => self.opening(portal, cell, level, shown),
            Direction::OutOf { portal } => {
                let cell = shown
                    .iter()
                    .find(|tile| tile.id == portal)
                    .map_or_else(|| Self::centre(self.field), |tile| tile.rect);
                self.closing(portal, cell, level, shown)
            }
        }
    }

    fn opening(
        &self,
        portal: TileId,
        cell: Rect,
        level: u8,
        shown: Vec<PaintedTile>,
    ) -> Vec<PaintedTile> {
        let now = Self::lerp(cell, self.field, level);
        let push = Zoom::new(cell, now);
        let carry = Zoom::new(self.field, now);
        let ghosts = self.ghosts.iter().map(|ghost| {
            let mut tile = ghost.clone();
            tile.rect = push.apply(tile.rect);
            tile.layer = 0;
            if tile.id == portal {
                tile.veil = level;
            }
            tile
        });
        let arriving = shown.into_iter().map(|mut tile| {
            tile.rect = carry.apply(tile.rect);
            tile.layer = 1;
            tile
        });
        ghosts.chain(arriving).collect()
    }

    fn closing(
        &self,
        portal: TileId,
        cell: Rect,
        level: u8,
        shown: Vec<PaintedTile>,
    ) -> Vec<PaintedTile> {
        let now = Self::lerp(self.field, cell, level);
        let carry = Zoom::new(cell, now);
        let pull = Zoom::new(self.field, now);
        let returning = shown.into_iter().map(|mut tile| {
            tile.rect = carry.apply(tile.rect);
            if tile.id == portal {
                tile.veil = u8::MAX - level;
            }
            tile
        });
        let count = self.ghosts.len();
        let ghosts = self
            .ghosts
            .iter()
            .enumerate()
            .map(|(index, ghost)| {
                let mut tile = ghost.clone();
                tile.rect = pull.apply(tile.rect);
                tile.grown = tile.grown.min(self.shrink(index, count));
                tile.layer = 1;
                tile
            })
            .filter(|tile| tile.grown > 0);
        returning.chain(ghosts).collect()
    }

    /// How much of its cell the old tile at `index` of `count` still fills:
    /// the outermost goes first and the innermost last, all gone before the
    /// zoom lands.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    fn shrink(&self, index: usize, count: usize) -> u8 {
        let start = Self::SHRINK_SPREAD * index as f32 / count.max(1) as f32;
        let gone = ((self.travel.progress() - start) / Self::SHRINK_SPAN).clamp(0.0, 1.0);
        let left = 1.0 - gone;
        let eased = left * left * 2.0f32.mul_add(-left, 3.0);
        (eased * 255.0).round() as u8
    }

    const fn centre(field: Rect) -> Rect {
        Rect::new(field.x + field.w / 2, field.y + field.h / 2, 0, 0)
    }

    /// Advance by `elapsed_ms`; returns whether the zoom has landed.
    fn step(&mut self, elapsed_ms: u16) -> bool {
        self.travel.step(elapsed_ms, Self::TRAVEL_MS);
        self.travel.is_full()
    }

    fn lerp(from: Rect, to: Rect, level: u8) -> Rect {
        let level = i32::from(level);
        let mix = |a: i32, b: i32| a + ((b - a) * level + 127 * (b - a).signum()) / 255;
        let (x, y) = (mix(from.x, to.x), mix(from.y, to.y));
        Rect::new(
            x,
            y,
            mix(from.right(), to.right()) - x,
            mix(from.bottom(), to.bottom()) - y,
        )
    }
}

#[derive(Default)]
pub struct TileTransition {
    journey: Option<Journey>,
    clock: Option<u64>,
}

impl TileTransition {
    const MAX_STEP_MS: u64 = 64;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            journey: None,
            clock: None,
        }
    }

    /// Begin the zoom `passage` calls for, from the old folder's tiles
    /// `ghosts` laid out in `field`. Any other passage ends a zoom under way.
    pub fn start(&mut self, passage: Passage, field: Rect, ghosts: Vec<PaintedTile>, now_ms: u64) {
        let direction = match passage {
            Passage::Into(portal) => ghosts
                .iter()
                .find(|ghost| ghost.id == portal)
                .map(|ghost| Direction::Into {
                    portal,
                    cell: ghost.rect,
                }),
            Passage::OutOf(portal) => Some(Direction::OutOf { portal }),
            Passage::Elsewhere => None,
        };
        self.journey = direction.map(|direction| Journey {
            direction,
            field,
            ghosts,
            travel: Fade::rising(),
        });
        self.clock = self.journey.as_ref().map(|_| now_ms);
    }

    pub fn advance(&mut self, now_ms: u64) {
        let (Some(then), Some(journey)) = (self.clock, self.journey.as_mut()) else {
            return;
        };
        let elapsed = now_ms.saturating_sub(then).min(Self::MAX_STEP_MS);
        let landed = journey.step(u16::try_from(elapsed).unwrap_or(u16::MAX));
        self.clock = Some(now_ms.max(then));
        if landed {
            self.finish();
        }
    }

    /// Land the zoom at once when the field it was laid out in has changed.
    pub fn keep_within(&mut self, field: Rect) {
        if self
            .journey
            .as_ref()
            .is_some_and(|journey| journey.field != field)
        {
            self.finish();
        }
    }

    pub fn finish(&mut self) {
        self.journey = None;
        self.clock = None;
    }

    #[must_use]
    pub const fn is_animating(&self) -> bool {
        self.journey.is_some()
    }

    /// Whether the folder now shown arrives with its tiles already grown.
    #[must_use]
    pub const fn arrives_grown(&self) -> bool {
        matches!(
            self.journey,
            Some(Journey {
                direction: Direction::OutOf { .. },
                ..
            })
        )
    }

    /// The tiles of the folder now shown, `shown`, as this frame of the zoom
    /// draws them, together with the old folder's tiles still in sight.
    #[must_use]
    pub fn stage(&self, shown: Vec<PaintedTile>) -> Vec<PaintedTile> {
        match &self.journey {
            Some(journey) => journey.stage(shown),
            None => shown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TileTransition;
    use crate::row_kind::RowKind;
    use crate::tile_action::TileAction;
    use crate::tile_id::TileId;
    use crate::tile_painter::PaintedTile;
    use crate::tile_passage::Passage;
    use platform::draw::{Color, Rect};

    const FIELD: Rect = Rect::new(0, 0, 1000, 600);
    const LEFT: Rect = Rect::new(0, 0, 600, 600);
    const RIGHT: Rect = Rect::new(600, 0, 400, 600);

    fn tile(name: &str, rect: Rect) -> PaintedTile {
        PaintedTile {
            id: TileId::of(name),
            rect,
            slot: 0,
            from_slot: 0,
            mix: u8::MAX,
            kind: RowKind::Leaf,
            name: name.to_owned(),
            detail: String::new(),
            name_color: Color::WHITE,
            chosen: 0,
            drop_target: false,
            raise: 0,
            reveal: 0,
            grown: u8::MAX,
            veil: 0,
            layer: 0,
            glow: [0; TileAction::ALL.len()],
            press: [0; TileAction::ALL.len()],
            tray: None,
        }
    }

    fn pair() -> Vec<PaintedTile> {
        vec![tile("big", LEFT), tile("dir", RIGHT)]
    }

    fn inner() -> Vec<PaintedTile> {
        vec![tile("in-a", LEFT), tile("in-b", RIGHT)]
    }

    fn named<'a>(staged: &'a [PaintedTile], name: &str) -> &'a PaintedTile {
        staged
            .iter()
            .find(|tile| tile.name == name)
            .expect("staged")
    }

    fn run(transition: &mut TileTransition, frames: u64) {
        for frame in 1..=frames {
            transition.advance(frame * 16);
        }
    }

    fn opening() -> TileTransition {
        let mut transition = TileTransition::new();
        transition.start(Passage::Into(TileId::of("dir")), FIELD, pair(), 0);
        transition
    }

    fn closing() -> TileTransition {
        let mut transition = TileTransition::new();
        transition.start(Passage::OutOf(TileId::of("dir")), FIELD, inner(), 0);
        transition
    }

    #[test]
    fn opening_a_folder_not_in_view_does_not_zoom() {
        let mut transition = TileTransition::new();
        transition.start(Passage::Into(TileId::of("gone")), FIELD, pair(), 0);
        assert!(!transition.is_animating());
        assert_eq!(transition.stage(inner()), inner());
    }

    #[test]
    fn elsewhere_does_not_zoom() {
        let mut transition = TileTransition::new();
        transition.start(Passage::Elsewhere, FIELD, pair(), 0);
        assert!(!transition.is_animating());
    }

    #[test]
    fn opening_starts_with_the_new_folder_inside_the_tile() {
        let transition = opening();
        assert!(!transition.arrives_grown());
        let staged = transition.stage(inner());
        assert_eq!(staged.len(), 4);
        assert_eq!(staged[1].name, "dir", "the old tiles are drawn first");
        assert_eq!(staged[1].veil, 0);
        let a = named(&staged, "in-a").rect;
        let b = named(&staged, "in-b").rect;
        assert_eq!((a.x, b.right(), a.h), (RIGHT.x, RIGHT.right(), RIGHT.h));
    }

    #[test]
    fn opening_expands_the_tile_into_the_background_and_pushes_the_rest_out() {
        let mut transition = opening();
        run(&mut transition, 12);
        let staged = transition.stage(inner());
        let portal = named(&staged, "dir");
        assert!(portal.rect.x < RIGHT.x && portal.rect.w > RIGHT.w, "{portal:?}");
        assert!(portal.veil > 0);
        let big = named(&staged, "big");
        assert_eq!(big.veil, 0);
        assert!(big.rect.right() <= portal.rect.x, "pushed left");
        let a = named(&staged, "in-a").rect;
        assert_eq!(a.x, portal.rect.x, "the new tiles fill the growing tile");
    }

    #[test]
    fn opening_lands_and_stops_asking_for_frames() {
        let mut transition = opening();
        run(&mut transition, 40);
        assert!(!transition.is_animating());
        assert_eq!(transition.stage(inner()), inner());
    }

    #[test]
    fn going_up_starts_with_the_folder_tile_as_the_background() {
        let transition = closing();
        assert!(transition.arrives_grown());
        let staged = transition.stage(pair());
        let portal = named(&staged, "dir");
        assert_eq!(portal.rect, FIELD);
        assert_eq!(portal.veil, u8::MAX);
        assert!(named(&staged, "big").rect.right() <= FIELD.x, "off the edge");
        assert_eq!(staged[3].name, "in-b", "the old tiles are drawn last");
    }

    #[test]
    fn going_up_shrinks_the_old_tiles_outermost_first_into_the_folder_tile() {
        let mut transition = closing();
        run(&mut transition, 10);
        let staged = transition.stage(pair());
        let portal = named(&staged, "dir");
        assert!(portal.rect.w < FIELD.w && portal.rect.w > RIGHT.w, "{portal:?}");
        assert!(portal.veil < u8::MAX && portal.veil > 0);
        let (a, b) = (named(&staged, "in-a"), named(&staged, "in-b"));
        assert!(a.grown < b.grown, "{} then {}", a.grown, b.grown);
        for ghost in [a, b] {
            assert!(ghost.rect.x >= portal.rect.x);
            assert!(ghost.rect.right() <= portal.rect.right());
        }
    }

    #[test]
    fn going_up_leaves_no_old_tile_before_it_lands() {
        let mut transition = closing();
        run(&mut transition, 23);
        assert!(transition.is_animating());
        assert_eq!(transition.stage(pair()).len(), 2);
    }

    #[test]
    fn a_new_field_lands_the_zoom() {
        let mut transition = closing();
        transition.keep_within(FIELD);
        assert!(transition.is_animating());
        transition.keep_within(Rect::new(0, 0, 900, 600));
        assert!(!transition.is_animating());
    }

    #[test]
    fn a_stalled_frame_resumes_instead_of_skipping_to_the_end() {
        let mut transition = closing();
        transition.advance(10_000);
        assert!(transition.is_animating());
    }
}
