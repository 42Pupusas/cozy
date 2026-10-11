//! [`TileScene`]: the tile view's state and animation, shared by every window
//! that shows a folder as a spiral.

use browse::DirectoryBrowser;
use platform::draw::{Point, Rect};
use widget::ListCommand;

use crate::hover::Spot;
use crate::spiral::MinCell;
use crate::spiral_cursor::{Landing, SpiralCursor};
use crate::tile_action::TileAction;
use crate::tile_field::{Page, PlacedMore, PlacedTile, TileField};
use crate::tile_growth::{Destination, TileGrowth};
use crate::tile_id::TileId;
use crate::tile_motion::TileMotion;

/// The room the spiral is laid out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flow {
    pub area: Rect,
    pub floor: i32,
    pub tail: Option<MinCell>,
}

/// What the glides are pointed at for one step.
#[derive(Debug, Clone, Copy)]
pub struct MotionAim<'a> {
    pub spot: Option<Spot>,
    pub cursor: Option<usize>,
    pub selected: &'a [usize],
    pub tray: Option<usize>,
    pub actions: bool,
    pub held: Option<(usize, TileAction)>,
}

/// Where a keyboard movement took the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Steering {
    Stay,
    Land(usize),
    Scrolled(Option<usize>),
}

pub struct TileScene {
    field: TileField,
    motion: TileMotion,
    growth: TileGrowth,
    clock_ms: u64,
}

impl TileScene {
    pub const FRAME_MS: i32 = 16;

    #[must_use]
    pub fn new(wake: waker::Waker) -> Self {
        Self {
            field: TileField::new(wake),
            motion: TileMotion::new(),
            growth: TileGrowth::new(),
            clock_ms: 0,
        }
    }

    #[must_use]
    pub const fn motion(&self) -> &TileMotion {
        &self.motion
    }

    #[must_use]
    pub const fn growth(&self) -> &TileGrowth {
        &self.growth
    }

    #[must_use]
    pub const fn field(&self) -> &TileField {
        &self.field
    }

    #[must_use]
    pub const fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    pub fn sync(&mut self, nav: &DirectoryBrowser) {
        self.field.sync(nav);
    }

    /// Fold in folder totals the size worker produced. Returns whether the
    /// tiles must be redrawn.
    pub fn poll(&mut self, nav: &DirectoryBrowser) -> bool {
        let arrived = self.field.poll();
        self.field.sync(nav);
        arrived
    }

    #[must_use]
    pub fn layout(&self, flow: &Flow) -> Vec<PlacedTile> {
        self.field.layout(flow.area, flow.floor, flow.tail)
    }

    #[must_use]
    pub fn more(&self, flow: &Flow) -> Option<PlacedMore> {
        self.field.more(flow.area, flow.floor, flow.tail)
    }

    #[must_use]
    pub fn card_cell(&self, flow: &Flow) -> Option<Rect> {
        self.field.card_cell(flow.area, flow.floor, flow.tail)
    }

    #[must_use]
    pub fn page(&self, flow: &Flow) -> Page {
        self.field.page(flow.area, flow.floor, flow.tail)
    }

    #[must_use]
    pub fn hit(&self, flow: &Flow, at: Point) -> Option<usize> {
        self.field.hit(flow.area, flow.floor, flow.tail, at)
    }

    #[must_use]
    pub fn more_at(&self, flow: &Flow, at: Point) -> bool {
        self.more(flow).is_some_and(|more| more.rect.contains(at))
    }

    #[must_use]
    pub fn placed_at(&self, flow: &Flow, at: Point) -> Option<PlacedTile> {
        self.layout(flow)
            .into_iter()
            .find(|placed| placed.rect.contains(at))
    }

    #[must_use]
    pub fn placed_item(&self, flow: &Flow, row: usize) -> Option<PlacedTile> {
        self.layout(flow)
            .into_iter()
            .find(|placed| placed.row == row)
    }

    /// Scroll the field by `steps` tiles and aim the growth at the new view.
    /// Returns whether the view moved.
    pub fn slide(&mut self, flow: &Flow, nav: &DirectoryBrowser, steps: i32) -> bool {
        let moved = self
            .field
            .slide(flow.area, flow.floor, flow.tail, steps);
        if moved {
            self.aim_growth(flow, nav);
        }
        moved
    }

    /// Scroll without re-aiming the growth, for a steer the caller finishes.
    pub fn slide_quietly(&mut self, flow: &Flow, steps: i32) -> bool {
        self.field.slide(flow.area, flow.floor, flow.tail, steps)
    }

    /// Resolve a movement key against the tiles in view, scrolling the field
    /// when the walk leaves them. After a scroll the row to put the cursor on
    /// is the tile that came in: the smallest in view when scrolling on one
    /// tile at a time, the largest after a longer jump, and the mirror of
    /// each going back.
    pub fn steer(&mut self, flow: &Flow, cursor: Option<usize>, command: ListCommand) -> Steering {
        let page = self.page(flow);
        let landing = SpiralCursor::new(&page.rows, page.later, page.earlier).land(cursor, command);
        match landing {
            Landing::At(row) => Steering::Land(row),
            Landing::Stay => Steering::Stay,
            Landing::Slide(steps) => {
                self.slide_quietly(flow, steps);
                Steering::Scrolled(self.entered_row(flow, steps))
            }
        }
    }

    fn entered_row(&self, flow: &Flow, steps: i32) -> Option<usize> {
        let page = self.page(flow);
        let forward = steps > 0;
        let single = steps.abs() == 1;
        if forward == single {
            page.rows.last().copied()
        } else {
            page.rows.first().copied()
        }
    }

    pub fn aim_growth(&mut self, flow: &Flow, nav: &DirectoryBrowser) {
        let targets = self.targets(flow, nav);
        self.growth.aim(nav.path(), flow.area, &targets, self.clock_ms);
    }

    pub fn reaim_if_primed(&mut self, flow: &Flow, nav: &DirectoryBrowser) {
        if self.growth.is_primed() {
            self.aim_growth(flow, nav);
        }
    }

    fn targets(&self, flow: &Flow, nav: &DirectoryBrowser) -> Vec<Destination> {
        let more = self.more(flow).map(|more| Destination {
            id: TileId::More,
            row: 0,
            rect: more.rect,
            slot: more.slot,
        });
        self.layout(flow)
            .iter()
            .filter_map(|placed| {
                let entry = nav.entry(placed.row)?;
                Some(Destination {
                    id: TileId::of(&entry.name),
                    row: placed.row,
                    rect: placed.rect,
                    slot: placed.slot,
                })
            })
            .chain(more)
            .collect()
    }

    /// Carry the glides on to `now_ms` and point them at `aim`. Returns
    /// whether a frame is owed: one while a glide is running, and the one
    /// that lands it.
    pub fn step(
        &mut self,
        flow: &Flow,
        nav: &DirectoryBrowser,
        aim: &MotionAim<'_>,
        now_ms: u64,
    ) -> bool {
        let was_moving = self.is_animating();
        self.clock_ms = now_ms;
        self.field.sync(nav);
        self.aim_growth(flow, nav);
        self.growth.advance(now_ms);
        self.motion.aim(
            aim.spot,
            aim.cursor,
            aim.selected,
            aim.tray,
            aim.actions,
            now_ms,
        );
        let held = aim
            .held
            .filter(|&(row, action)| aim.spot == Some(Spot::Action(row, action)));
        self.motion.press_on(held, now_ms);
        self.motion.advance(now_ms);
        was_moving || self.is_animating()
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.motion.is_animating() || self.growth.is_animating()
    }

    #[must_use]
    pub fn frame_wait_ms(&self) -> Option<i32> {
        self.is_animating().then_some(Self::FRAME_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::{Flow, MotionAim, Steering, TileScene};
    use widget::{Direction, ListCommand};
    use crate::hover::{Pane, Spot};
    use crate::test_support::TempDir;
    use browse::DirectoryBrowser;
    use platform::draw::{Point, Rect};

    const AREA: Rect = Rect::new(0, 0, 1300, 800);

    fn flow(floor: i32) -> Flow {
        Flow {
            area: AREA,
            floor,
            tail: None,
        }
    }

    fn idle() -> MotionAim<'static> {
        MotionAim {
            spot: None,
            cursor: None,
            selected: &[],
            tray: None,
            actions: true,
            held: None,
        }
    }

    fn scene() -> TileScene {
        TileScene::new(waker::Waker::new().expect("eventfd"))
    }

    fn folder(name: &str, files: usize) -> (DirectoryBrowser, TempDir) {
        let dir = TempDir::new(name);
        for i in 0..files {
            dir.create_file(&format!("f{i:02}"), &vec![0; 100 + i]);
        }
        let nav = DirectoryBrowser::open(dir.path(), 800, 600, 28).expect("open");
        (nav, dir)
    }

    fn settle(scene: &mut TileScene, flow: &Flow, nav: &DirectoryBrowser) -> u64 {
        let mut now = 0;
        while scene.step(flow, nav, &idle(), now) {
            now += 16;
            assert!(now < 20_000, "the glides never settled");
        }
        now
    }

    #[test]
    fn a_fresh_scene_owes_no_frame() {
        assert!(!scene().is_animating());
        assert_eq!(scene().frame_wait_ms(), None);
    }

    #[test]
    fn a_gliding_scene_asks_to_be_woken_each_frame() {
        let (nav, _dir) = folder("scene_wait", 5);
        let flow = flow(1);
        let mut scene = scene();
        scene.step(&flow, &nav, &idle(), 0);
        assert_eq!(scene.frame_wait_ms(), Some(TileScene::FRAME_MS));
    }

    #[test]
    fn the_first_step_in_a_folder_grows_its_tiles_and_then_settles() {
        let (nav, _dir) = folder("scene_grow", 5);
        let flow = flow(1);
        let mut scene = scene();
        assert!(scene.step(&flow, &nav, &idle(), 0));
        assert!(scene.growth().is_primed());
        assert!(settle(&mut scene, &flow, &nav) > 0);
        assert!(!scene.is_animating());
    }

    #[test]
    fn every_entry_gets_a_tile_when_the_room_is_large() {
        let (nav, _dir) = folder("scene_layout", 5);
        let mut scene = scene();
        scene.sync(&nav);
        assert_eq!(scene.layout(&flow(1)).len(), 5);
        assert!(scene.more(&flow(1)).is_none());
    }

    #[test]
    fn a_point_inside_a_tile_hits_its_row() {
        let (nav, _dir) = folder("scene_hit", 5);
        let flow = flow(1);
        let mut scene = scene();
        scene.sync(&nav);
        let placed = scene.layout(&flow)[2];
        let centre = Point::new(
            placed.rect.x + placed.rect.w / 2,
            placed.rect.y + placed.rect.h / 2,
        );
        assert_eq!(scene.hit(&flow, centre), Some(placed.row));
        assert_eq!(scene.placed_at(&flow, centre), Some(placed));
        assert_eq!(scene.placed_item(&flow, placed.row), Some(placed));
    }

    #[test]
    fn a_crowded_folder_offers_a_more_tile_that_answers_a_hit() {
        let (nav, _dir) = folder("scene_more", 40);
        let flow = flow(120);
        let mut scene = scene();
        scene.sync(&nav);
        let more = scene.more(&flow).expect("more tile");
        let centre = Point::new(more.rect.x + more.rect.w / 2, more.rect.y + more.rect.h / 2);
        assert!(scene.more_at(&flow, centre));
        assert!(!scene.more_at(&flow, Point::new(-5, -5)));
    }

    #[test]
    fn sliding_aims_the_growth_at_the_new_view() {
        let (nav, _dir) = folder("scene_slide", 40);
        let flow = flow(120);
        let mut scene = scene();
        scene.step(&flow, &nav, &idle(), 0);
        let before = scene.page(&flow).rows;
        assert!(scene.slide(&flow, &nav, 1));
        assert_ne!(scene.page(&flow).rows, before);
        assert!(scene.is_animating());
    }

    #[test]
    fn stepping_down_inside_the_view_lands_on_the_next_tile_in_rank_order() {
        let (nav, _dir) = folder("scene_steer_in", 5);
        let flow = flow(1);
        let mut scene = scene();
        scene.sync(&nav);
        let rows = scene.page(&flow).rows;
        let down = ListCommand::Move(Direction::Down);
        assert_eq!(scene.steer(&flow, Some(rows[0]), down), Steering::Land(rows[1]));
    }

    #[test]
    fn stepping_past_the_last_tile_of_a_fitting_folder_stays() {
        let (nav, _dir) = folder("scene_steer_stay", 5);
        let flow = flow(1);
        let mut scene = scene();
        scene.sync(&nav);
        let rows = scene.page(&flow).rows;
        let down = ListCommand::Move(Direction::Down);
        let last = *rows.last().expect("tiles");
        assert_eq!(scene.steer(&flow, Some(last), down), Steering::Stay);
    }

    #[test]
    fn stepping_off_the_end_of_a_crowded_view_scrolls_and_names_the_tile_that_came_in() {
        let (nav, _dir) = folder("scene_steer_scroll", 40);
        let flow = flow(120);
        let mut scene = scene();
        scene.sync(&nav);
        let before = scene.page(&flow).rows;
        let last = *before.last().expect("tiles");
        let down = ListCommand::Move(Direction::Down);
        let steered = scene.steer(&flow, Some(last), down);
        let after = scene.page(&flow).rows;
        assert_ne!(before, after);
        assert_eq!(steered, Steering::Scrolled(after.last().copied()));
    }

    #[test]
    fn sliding_past_the_end_does_nothing() {
        let (nav, _dir) = folder("scene_slide_end", 3);
        let flow = flow(1);
        let mut scene = scene();
        scene.sync(&nav);
        assert!(!scene.slide(&flow, &nav, 1));
        assert!(!scene.slide_quietly(&flow, -1));
    }

    #[test]
    fn pointing_at_a_tile_raises_it_and_the_step_reports_the_glide() {
        let (nav, _dir) = folder("scene_raise", 5);
        let flow = flow(1);
        let mut scene = scene();
        let now = settle(&mut scene, &flow, &nav);
        let aim = MotionAim {
            spot: Some(Spot::Item(Pane::Tiles, 1)),
            ..idle()
        };
        assert!(scene.step(&flow, &nav, &aim, now + 16));
        assert!(scene.motion().is_animating());
    }

    #[test]
    fn a_step_remembers_the_time_it_was_carried_to() {
        let (nav, _dir) = folder("scene_clock", 2);
        let mut scene = scene();
        scene.step(&flow(1), &nav, &idle(), 4242);
        assert_eq!(scene.clock_ms(), 4242);
    }

    #[test]
    fn reaiming_waits_until_the_growth_has_been_primed() {
        let (nav, _dir) = folder("scene_reaim", 3);
        let flow = flow(1);
        let mut scene = scene();
        scene.sync(&nav);
        scene.reaim_if_primed(&flow, &nav);
        assert!(!scene.growth().is_primed());
        scene.aim_growth(&flow, &nav);
        assert!(scene.growth().is_primed());
    }
}
