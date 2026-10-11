//! [`FilesWindow`] -- the [`Painter`] that draws a [`Browser`] and routes
//! compositor input into it.
//!
//! This is the only module in the binary that knows about pixels or Wayland,
//! and it holds no application state of its own beyond what drawing needs:
//! every decision it makes was already made by `Browser`. That is the payoff
//! of the layering -- the file-manager behaviour is tested headlessly, and
//! what is left here is a straight transcription onto a canvas.

use platform::draw::{Canvas, Color, Point, Rect};
use platform::protocol::PointerButton;
use platform::toplevel::{Key, KeyPress, Painter, PhysicalSize, PointerEventKind, Position};
use std::os::fd::RawFd;
use widget::{DragArm, Mods};

use crate::action_tray::{ActionTray, TrayOpen};
use crate::breadcrumb::Breadcrumb;
use crate::browser::{Browser, Outcome};
use crate::card_surface::{Backdrop, CardSurface};
use crate::drag_badge::{BadgeStyle, DragBadge};
use crate::favorites::Favorites;
use crate::frame::Frame;
use crate::hover::{Hover, Pane, Spot};
use tiles::pinned_card::PinnedCard;
use crate::row_format::RowFormat;
use tiles::row_kind::RowKind;
use crate::sidebar::{Sidebar, SidebarRow};
use tiles::spiral::MinCell;
use tiles::spiral_cursor::{Landing, SpiralCursor};
use volumes::mount_backend::MountOutcome;
use volumes::{AttachedDevice, VolumeFlush, VolumeWatch};

use crate::mount_service::MountProgress;
use crate::mount_session::{MountIdentity, MountSession};
use crate::password_panel::{PanelStyle, PanelText, PasswordPanel};
use crate::storage::Storage;
use tiles::theme::Theme;
use tiles::tile_action::TileAction;
use tiles::tile_field::{PlacedMore, PlacedTile, TileField};
use tiles::tile_geometry::TileGeometry;
use tiles::tile_growth::{Destination, Shape, TileGrowth};
use tiles::tile_id::TileId;
use crate::tile_motion::{TileKey, TileMotion};
use crate::tile_painter::{PaintedMove, PaintedTile, PaintedTray, TilePainter, TileRename};
use crate::toolbar::{Target, Toolbar};
use tiles::typography::Typography;

/// Milliseconds within which a second click counts as a double-click.
///
/// 400ms is the long-standing X11/GTK default. Worth stating rather than
/// tuning: a threshold that is too short makes double-clicking feel broken
/// for anyone who does not click quickly, which includes most people.
const DOUBLE_CLICK_MS: u64 = 400;

/// Pixels the pointer may move between two clicks and still double-click.
///
/// Without a distance check, clicking two *different* rows in quick
/// succession registers as a double-click on the second and opens a file the
/// user never meant to open.
const DOUBLE_CLICK_SLOP: i32 = 4;

/// The interval between frames while a tile is gliding, about sixty a second.
const FRAME_MS: i32 = 16;

/// The inline rename box's contents for one frame.
///
/// Gathered before painting: drawing borrows `self.type_scale` mutably, which
/// cannot coexist with a borrow of `self.browser`.
struct PaintedRename {
    row: usize,
    text: String,
    before_caret: String,
    valid: bool,
}

/// Draws a browser and feeds it input.
pub struct FilesWindow {
    browser: Browser,
    theme: Theme,
    /// The type scale. Held by value because `TextRenderer` caches rasterized
    /// glyphs and so needs `&mut` to draw -- which is why the paint helpers
    /// below take `&mut self` rather than `&self`.
    type_scale: Typography,
    sidebar: Sidebar,
    favorites: Favorites,
    size: PhysicalSize,
    focused: bool,
    /// Which list row the pointer is over. See [`Hover`] for why this is a
    /// type rather than an `Option<usize>`.
    hover: Hover,
    /// Set when the user asks to quit, read by the event loop.
    quit: bool,
    /// A file the user activated, for the loop to hand to the system opener.
    ///
    /// Queued rather than spawned here: launching a process from inside a
    /// paint callback would run it while the window is mid-frame.
    pending_open: Option<std::path::PathBuf>,
    /// When and where the last left-click landed, for double-click detection.
    last_click: Option<(u64, Position)>,
    /// Disk usage for the open directory's filesystem, shown in the
    /// sidebar's footer.
    ///
    /// Cached rather than read per frame: `statvfs` is a syscall, and at 60fps
    /// a per-paint read would be thousands of pointless syscalls a minute for
    /// a figure that changes on the scale of minutes. Refreshed when the
    /// directory changes, which is also when it can become a *different*
    /// filesystem.
    storage: Option<Storage>,
    /// Tracks whether the left button's current press has moved far enough
    /// to become a drag -- see [`widget::DragArm`]. A plain click must never
    /// start a drag, so this only fires past a real pixel threshold, and
    /// only for a press that actually landed on a row (see
    /// [`Self::drag_row`]).
    drag_arm: DragArm,
    /// The row a left-button press landed on, and its pane-relative
    /// coordinates, kept from press until either the drag threshold fires or
    /// the button is released. `None` whenever no row is the candidate for a
    /// drag -- a press on empty space, the scrollbar, or the toolbar.
    drag_row: Option<usize>,
    /// Paths queued for the event loop to hand to `Toplevel::start_drag`.
    ///
    /// Queued rather than started here for the same reason as
    /// `pending_open`: `Painter::on_pointer` has no access to the `Toplevel`
    /// that owns `start_drag` (it borrows the painter, not the other way
    /// around), so the loop drains this once per tick.
    pending_drag: Option<Vec<std::path::PathBuf>>,
    /// Which row (if any) an incoming drag is currently hovering, for a
    /// drop-target highlight. Set by the event loop each tick from
    /// `Toplevel::is_drag_over`/`drag_position`, since that state lives on
    /// the `Toplevel`, not a callback `Painter` receives on every motion.
    drop_hover_row: Option<usize>,
    /// The floating "N items" label shown while a drag is in flight.
    drag_badge: DragBadge,
    /// The mount in flight, and the identity mounts run as.
    mount: MountSession,
    /// The device a Mount action would attach.
    ///
    /// Held rather than re-derived at the moment of the action: the scan that
    /// produced it walks `/sys` and udev, and the sidebar row the user
    /// clicked is the thing that names it.
    #[allow(dead_code)]
    mount_candidate: Option<AttachedDevice>,
    /// Watches the mount table so attached devices appear and disappear.
    ///
    /// Held here rather than in the event loop because the sidebar it feeds
    /// lives here, and a poll that found a change would otherwise have to
    /// hand the new set across the same seam the loop already crosses for
    /// drags and opens.
    volumes: VolumeWatch,
    /// The tile presentation: size ranking and overflow pages.
    tiles: TileField,
    /// The glides tiles make as the pointer arrives and leaves.
    motion: TileMotion,
    /// Tiles growing out of the spiral as a folder or page appears.
    growth: TileGrowth,
    /// The time the glides were last carried to, for aiming them at a layout
    /// that changed between two ticks.
    motion_ms: u64,
    /// The tile button the left button is held down on. It fires on release,
    /// and only if the pointer is still on it.
    pressed_action: Option<(usize, TileAction)>,
    /// The tray a right click opened, if one is open.
    tray: Option<TrayOpen>,
    /// Text queued for the system clipboard, for the loop to hand to the
    /// `Toplevel` that owns it.
    pending_copy: Option<String>,
}

impl FilesWindow {
    #[must_use]
    pub fn new(
        browser: Browser,
        theme: Theme,
        type_scale: Typography,
        sidebar: Sidebar,
        favorites: Favorites,
        size: PhysicalSize,
        wake: waker::Waker,
    ) -> Self {
        Self {
            volumes: VolumeWatch::new(),
            tiles: TileField::new(wake.clone()),
            motion: TileMotion::new(),
            growth: TileGrowth::new(),
            motion_ms: 0,
            pressed_action: None,
            tray: None,
            pending_copy: None,
            mount: MountSession::new(wake, MountIdentity::current()),
            mount_candidate: None,
            browser,
            theme,
            type_scale,
            sidebar,
            favorites,
            size,
            focused: false,
            hover: Hover::none(),
            quit: false,
            pending_open: None,
            last_click: None,
            // Left empty so `new` can stay const; the first paint fills it.
            storage: None,
            drag_arm: DragArm::new(),
            drag_row: None,
            pending_drag: None,
            drop_hover_row: None,
            drag_badge: DragBadge::new(),
        }
    }

    /// Take any text a tile button queued for the system clipboard, for the
    /// event loop to hand to `Toplevel::copy_text`.
    pub const fn take_pending_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    /// [`Self::step_motion`] at the current time, for the event loop.
    pub fn advance_motion(&mut self) -> bool {
        self.step_motion(Self::now_ms())
    }

    /// Carry the tile glides on to `now_ms` and point them at what is under
    /// the pointer. Returns whether a frame is owed: one while a glide is
    /// running, and the one that lands it.
    ///
    /// Stepped by the loop before each tick rather than inside `paint`, so
    /// painting stays a pure function of state and a test can choose the
    /// time. The first frame of a glide is always its start, so aiming one
    /// pass behind the pointer is invisible.
    pub fn step_motion(&mut self, now_ms: u64) -> bool {
        let was_moving = self.motion.is_animating() || self.growth.is_animating();
        self.motion_ms = now_ms;
        self.step_growth(now_ms);
        let cursor = self.focused.then(|| self.browser.pane().cursor()).flatten();
        let tray = self.open_tray().map(|(row, _)| row);
        self.motion.aim(
            self.hover.spot(),
            cursor,
            self.browser.pane().selection().selected(),
            tray,
            !self.browser.is_renaming() && !self.browser.is_moving(),
            now_ms,
        );
        let held = self
            .pressed_action
            .filter(|&(row, action)| self.hover.spot() == Some(Spot::Action(row, action)));
        self.motion.press_on(held, now_ms);
        self.motion.advance(now_ms);
        was_moving || self.motion.is_animating() || self.growth.is_animating()
    }

    fn step_growth(&mut self, now_ms: u64) {
        self.sync_tiles();
        self.aim_growth(now_ms);
        self.growth.advance(now_ms);
    }

    fn aim_growth(&mut self, now_ms: u64) {
        let targets = self.growth_targets();
        let bounds = self.frame().field();
        self.growth
            .aim(self.browser.path(), bounds, &targets, now_ms);
    }

    fn growth_targets(&self) -> Vec<Destination> {
        let more = self.tile_more().map(|more| Destination {
            id: TileId::More,
            row: 0,
            rect: more.rect,
            slot: more.slot,
        });
        self.tile_layout()
            .iter()
            .filter_map(|placed| {
                let entry = self.browser.entry(placed.row)?;
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

    /// How long the loop may sleep before the next glide frame is due, or
    /// `None` when nothing is moving.
    #[must_use]
    pub fn motion_wait_ms(&self) -> Option<i32> {
        (self.motion.is_animating() || self.growth.is_animating()).then_some(FRAME_MS)
    }

    /// Take any drag the last pointer gesture started, for the event loop to
    /// hand to `Toplevel::start_drag`.
    pub const fn take_pending_drag(&mut self) -> Option<Vec<std::path::PathBuf>> {
        self.pending_drag.take()
    }

    /// Tell the window where an incoming drag currently sits, so the row (if
    /// any) under it can be highlighted as the drop target.
    ///
    /// `over` is [`platform::toplevel::Toplevel::is_drag_over`] and `at` is
    /// [`platform::toplevel::Toplevel::drag_position`] -- both live on the
    /// `Toplevel`, not delivered to `Painter` as a callback, so the event
    /// loop reports them here once per tick instead.
    pub fn update_drag_hover(&mut self, over: bool, at: (i32, i32)) {
        self.sync_tiles();
        self.drop_hover_row = if over {
            self.browser.navigable_row(self.entry_at(at))
        } else {
            None
        };
        if over {
            self.drag_badge.follow(at);
        }
    }

    /// Report whether a drag this window started is still in flight.
    ///
    /// Driven from the event loop rather than inferred here: once the
    /// compositor takes the pointer grab this window stops receiving the
    /// release, so the painter has no way of its own to learn that the
    /// gesture ended. Without this the badge would stay on screen forever.
    pub const fn update_drag_active(&mut self, dragging: bool) {
        if !dragging {
            self.drag_badge.end();
        }
    }

    /// The entry whose tile lies under window position `at`.
    fn entry_at(&self, at: (i32, i32)) -> Option<usize> {
        self.tile_at(Point::new(at.0, at.1))
    }

    /// The field the spiral fills and the room the card needs in it, so
    /// every layout, hit-test and page query sees one shape.
    fn flow_parts(&self) -> (Rect, Option<MinCell>) {
        let frame = self.frame();
        (frame.field(), frame.card_need())
    }

    /// The pinned card, fitted to the spiral's innermost cell.
    fn card(&self) -> Option<PinnedCard> {
        let frame = self.frame();
        let need = frame.card_need()?;
        let cell =
            self.tiles
                .card_cell(frame.field(), self.theme.spacing.tile_floor(), Some(need))?;
        Some(frame.card_in(cell))
    }

    fn tile_layout(&self) -> Vec<PlacedTile> {
        let (spiral, tail) = self.flow_parts();
        self.tiles
            .layout(spiral, self.theme.spacing.tile_floor(), tail)
    }

    fn tile_more(&self) -> Option<PlacedMore> {
        let (spiral, tail) = self.flow_parts();
        self.tiles
            .more(spiral, self.theme.spacing.tile_floor(), tail)
    }

    fn more_at(&self, at: Position) -> bool {
        self.tile_more()
            .is_some_and(|more| more.rect.contains(Point::new(at.x, at.y)))
    }

    fn tile_at(&self, at: Point) -> Option<usize> {
        let (spiral, tail) = self.flow_parts();
        self.tiles
            .hit(spiral, self.theme.spacing.tile_floor(), tail, at)
    }

    /// Bring the tile ranking up to date with the browser.
    fn sync_tiles(&mut self) {
        self.tiles.sync(self.browser.navigator());
    }

    /// Drain the directory watcher through the browser, re-reading the open
    /// directory when a change was reported. Returns whether anything is
    /// newly visible -- the same contract [`Self::poll_tiles`] keeps.
    pub fn poll_listing(&mut self) -> bool {
        self.browser.poll_listing()
    }

    /// The open directory's watched inotify fd, for the event loop's
    /// `poll(2)`. `None` while nothing is armed.
    #[must_use]
    pub fn watch_fd(&self) -> Option<RawFd> {
        self.browser.watch_fd()
    }

    /// The mount table descriptor, for the event loop to wait on.
    #[must_use]
    pub fn mounts_fd(&self) -> Option<RawFd> {
        self.volumes.mounts_fd()
    }

    /// The udev block-record descriptor, for the event loop to wait on.
    #[must_use]
    pub fn udev_fd(&self) -> Option<RawFd> {
        self.volumes.udev_fd()
    }

    /// Pass on which device descriptors the last wait found ready.
    pub fn absorb_device_signals(&mut self, wakeup: crate::idle_wait::Wakeup) {
        self.volumes.absorb(wakeup.mounts, wakeup.udev);
    }

    /// Fold in folder totals the size worker produced. Returns whether the
    /// tiles must be redrawn.
    pub fn poll_tiles(&mut self) -> bool {
        let arrived = self.tiles.poll();
        self.tiles.sync(self.browser.navigator());
        arrived
    }

    /// Whether the up control has somewhere to go.
    fn can_step_out(&self) -> bool {
        self.browser.can_go_up()
    }

    fn step_out(&mut self) -> Outcome {
        self.browser.act(crate::keymap::Action::GoUp)
    }

    /// A movement key in the tile view: walk the spiral in rank order,
    /// scrolling the field when the walk leaves the tiles in view.
    fn steer_tiles(&mut self, command: widget::ListCommand) -> Outcome {
        let (spiral, tail) = self.flow_parts();
        let page = self
            .tiles
            .page(spiral, self.theme.spacing.tile_floor(), tail);
        let cursor = self.browser.pane().cursor();
        let landing = SpiralCursor::new(&page.rows, page.later, page.earlier).land(cursor, command);
        let extend = matches!(command, widget::ListCommand::Extend(_));
        match landing {
            Landing::At(row) => self.browser.steer_to(row, extend),
            Landing::Slide(steps) => {
                self.tiles
                    .slide(spiral, self.theme.spacing.tile_floor(), tail, steps);
                self.steer_to_view_edge(steps, extend)
            }
            Landing::Stay => Outcome::Ignored,
        }
    }

    /// After the field scrolled by `steps`, put the cursor on the tile that
    /// came in: the smallest in view when scrolling on one tile at a time,
    /// the largest after a longer jump, and the mirror of each going back.
    fn steer_to_view_edge(&mut self, steps: i32, extend: bool) -> Outcome {
        let (spiral, tail) = self.flow_parts();
        let page = self
            .tiles
            .page(spiral, self.theme.spacing.tile_floor(), tail);
        let forward = steps > 0;
        let single = steps.abs() == 1;
        let edge = if forward == single {
            page.rows.last()
        } else {
            page.rows.first()
        };
        if let Some(&row) = edge {
            let _ = self.browser.steer_to(row, extend);
        }
        Outcome::Redraw
    }

    /// A left press anywhere in the window.
    ///
    /// The card's controls are checked before anything else, and a click on
    /// the card goes no further, so it cannot fall through to an entry lying
    /// under it.
    fn press_left(&mut self, at: Position, mods: Mods) {
        if self.press_in_tray(at) {
            return;
        }
        if let Some(target) = self.toolbar_hit(at) {
            self.activate_control(target);
            return;
        }
        let over_card = self
            .card()
            .is_some_and(|card| card.bounds().contains(Point::new(at.x, at.y)));
        if !over_card && self.frame().field().contains(Point::new(at.x, at.y)) {
            self.press_tile(at, mods);
        }
    }

    /// The tray a right click opened, laid out under its tile, or `None`
    /// when none is open or the tile is no longer on the page.
    fn open_tray(&self) -> Option<(usize, ActionTray)> {
        let open = self.tray?;
        if open.listing != self.browser.listing_revision() {
            return None;
        }
        let placed = self
            .tile_layout()
            .into_iter()
            .find(|placed| placed.row == open.row)?;
        let spacing = self.theme.spacing;
        let geometry = TileGeometry::of(placed.rect, spacing, self.type_scale.tile_lines());
        let tray = ActionTray::fit(
            &geometry.bar,
            geometry.face,
            self.frame().field(),
            spacing,
            self.type_scale.detail.line_height(),
        )?;
        Some((open.row, tray))
    }

    /// The card of the tile at `row` as drawn now, grown to hold `tray`.
    fn tray_card(&self, row: usize, tray: &ActionTray) -> Rect {
        let lift = TilePainter::lift(self.theme.spacing, self.motion.raise(TileKey::Item(row)));
        tray.card(lift, self.motion.tray(row))
    }

    /// Whether `at` is on the card of the tile with the tray open.
    fn on_open_card(&self, at: Position) -> bool {
        self.open_tray()
            .is_some_and(|(row, tray)| self.tray_card(row, &tray).contains(Point::new(at.x, at.y)))
    }

    /// The placed tile at `row`.
    fn placed_item(&self, row: usize) -> Option<PlacedTile> {
        self.tile_layout()
            .into_iter()
            .find(|placed| placed.row == row)
    }

    /// Deal with a left press while a tray is open. A press on the card never
    /// reaches the tiles; one outside closes the rows and carries on.
    fn press_in_tray(&mut self, at: Position) -> bool {
        if self.open_tray().is_none() {
            self.tray = None;
            return false;
        }
        if self.on_open_card(at) {
            self.pressed_action = self.live_action_at(at);
            return true;
        }
        self.tray = None;
        false
    }

    /// A right press: open the tray under the tile, or close it.
    fn press_right(&mut self, at: Position) {
        let point = Point::new(at.x, at.y);
        if self.on_open_card(at) {
            return;
        }
        self.tray = None;
        let over_card = self
            .card()
            .is_some_and(|card| card.bounds().contains(point));
        if over_card || self.toolbar_hit(at).is_some() {
            return;
        }
        if let Some(row) = self.tile_at(point) {
            let outcome = self.browser.pick(row, Mods::none());
            self.apply(outcome);
            self.tray = Some(TrayOpen {
                row,
                listing: self.browser.listing_revision(),
            });
        }
    }

    /// A left press in the tile field.
    ///
    /// A single click only selects. A second click on the same tile within
    /// the double-click window opens it: a folder is entered, a file is
    /// opened. Pressing and dragging carries the entry instead.
    fn press_tile(&mut self, at: Position, mods: Mods) {
        if let Some(held) = self.live_action_at(at) {
            self.pressed_action = Some(held);
            return;
        }
        if self.more_at(at) {
            self.show_next_tiles(at);
            return;
        }
        let now = Self::now_ms();
        let doubled = self.is_double_click(now, at);
        match self.tile_at(Point::new(at.x, at.y)) {
            None => {
                let outcome = self.browser.clear_selection();
                self.apply(outcome);
            }
            Some(row) => {
                if doubled && mods == Mods::none() {
                    let outcome = self.browser.activate_entry(row);
                    self.apply(outcome);
                    return;
                }
                let outcome = self.browser.pick(row, mods);
                self.apply(outcome);
                self.drag_row = Some(row);
                self.drag_arm.press((at.x, at.y));
            }
        }
    }

    fn show_next_tiles(&mut self, at: Position) {
        let shown = i32::try_from(self.tile_layout().len()).unwrap_or(i32::MAX);
        self.slide_tiles(shown.max(1), at);
    }

    fn slide_tiles(&mut self, steps: i32, at: Position) -> bool {
        let (spiral, tail) = self.flow_parts();
        let moved = self
            .tiles
            .slide(spiral, self.theme.spacing.tile_floor(), tail, steps);
        if moved {
            self.tray = None;
            self.aim_growth(self.motion_ms);
            let spot = self.spot_at(at);
            let _ = self.hover.move_to(spot);
        }
        moved
    }

    /// Run the tile button a press began on, if the pointer is still on it.
    fn release_action(&mut self, at: Position) {
        let Some(held) = self.pressed_action.take() else {
            return;
        };
        if self.live_action_at(at) == Some(held) {
            self.run_action(held.0, held.1);
        }
    }

    /// The tiles in view, gathered for painting: those shrinking away first,
    /// then the ones the layout holds, each drawn where its glide has got to.
    fn painted_tiles(&self, placed: &[PlacedTile]) -> Vec<PaintedTile> {
        let leaving = self
            .growth
            .leaving()
            .into_iter()
            .filter_map(|(id, row, shape)| {
                let entry = self.browser.entry(row)?;
                (TileId::of(&entry.name) == id).then(|| self.painted_entry_tile(row, shape))?
            });
        let held = placed.iter().filter_map(|placed| {
            let entry = self.browser.entry(placed.row)?;
            let shape = self
                .growth
                .shape(TileId::of(&entry.name))
                .unwrap_or_else(|| Shape::resting(placed.rect, placed.slot));
            self.painted_entry_tile(placed.row, shape)
        });
        let more = self.tile_more().map(|more| {
            let shape = self
                .growth
                .shape(TileId::More)
                .unwrap_or_else(|| Shape::resting(more.rect, more.slot));
            self.painted_more_tile(&more, shape)
        });
        leaving.chain(held).chain(more).collect()
    }

    fn painted_more_tile(&self, more: &PlacedMore, shape: Shape) -> PaintedTile {
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
            raise: self.motion.raise(TileKey::More),
            reveal: 0,
            grown: shape.grown,
            glow: [0; TileAction::ALL.len()],
            press: [0; TileAction::ALL.len()],
            tray: None,
        }
    }

    fn painted_entry_tile(&self, row: usize, shape: Shape) -> Option<PaintedTile> {
        let entry = self.browser.entry(row)?;
        let key = TileKey::Item(row);
        let sizes = self.tiles.sizes();
        let detail = if sizes.is_pending(entry) {
            String::from("measuring…")
        } else {
            RowFormat::human_size(sizes.of(entry))
        };
        let cut = self.browser.clipboard().is_cut(&entry.path);
        Some(PaintedTile {
            rect: shape.rect,
            slot: shape.slot,
            from_slot: shape.from_slot,
            mix: shape.mix,
            kind: RowKind::of(entry),
            name: entry.name.clone(),
            detail,
            name_color: if cut {
                self.theme.muted
            } else {
                RowFormat::color(entry, &self.theme)
            },
            chosen: self.motion.chosen(row),
            drop_target: self.drop_hover_row == Some(row),
            raise: self.motion.raise(key),
            reveal: self.motion.reveal(key),
            grown: shape.grown,
            glow: TileAction::ALL.map(|action| self.motion.glow(row, action)),
            press: TileAction::ALL.map(|action| self.motion.press(row, action)),
            tray: self.painted_tray(row),
        })
    }

    fn tile_rename(&self, placed: &[PlacedTile]) -> Option<TileRename> {
        let editing = self.painted_rename()?;
        let tile = placed.iter().find(|placed| placed.row == editing.row)?;
        Some(TileRename {
            tile: tile.rect,
            text: editing.text,
            before_caret: editing.before_caret,
            valid: editing.valid,
            raise: self.motion.raise(TileKey::Item(editing.row)),
        })
    }

    fn paint_tiles(&mut self, canvas: &mut Canvas<'_>) {
        if self.growth.is_primed() {
            self.aim_growth(self.motion_ms);
        }
        let placed = self.tile_layout();
        let tiles = self.painted_tiles(&placed);
        let rename = self.tile_rename(&placed);
        let mut brush = TilePainter::new(&self.theme, &mut self.type_scale);
        brush.paint(canvas, &tiles);
        if let Some(rename) = rename {
            brush.paint_rename(canvas, &rename);
        }
    }

    /// The rows the tile at `row` has opened, if it is the one a right click
    /// opened them on.
    fn painted_tray(&self, row: usize) -> Option<PaintedTray> {
        if self.tray.is_none_or(|open| open.row != row) {
            return None;
        }
        let (_, tray) = self.open_tray()?;
        let caption = match self.hover.spot() {
            Some(Spot::Action(at, action)) if at == row => action.label().to_owned(),
            _ => self.browser.name_of(row).unwrap_or_default(),
        };
        Some(PaintedTray {
            tray,
            level: self.motion.tray(row),
            caption,
        })
    }

    /// The "move to" box's contents, while it has the keyboard.
    fn painted_move(&self) -> Option<PaintedMove> {
        if !self.browser.is_moving() {
            return None;
        }
        let prompt = self.browser.move_prompt();
        let input = prompt.input()?;
        Some(PaintedMove {
            label: format!("Move {} to", prompt.subject()?),
            text: input.text().to_owned(),
            before_caret: input.before_caret().to_owned(),
            valid: self.browser.move_target_is_valid(),
        })
    }

    fn paint_move_prompt(&mut self, canvas: &mut Canvas<'_>) {
        let Some(prompt) = self.painted_move() else {
            return;
        };
        let field = self.frame().field();
        TilePainter::new(&self.theme, &mut self.type_scale).paint_move(canvas, field, &prompt);
    }

    /// Re-read the filesystem figures behind the sidebar footer.
    ///
    /// Called on navigation rather than per paint: crossing into a different
    /// mount is the only thing that can change which filesystem is being
    /// reported, and the fill level moves far slower than the frame rate.
    fn refresh_storage(&mut self) {
        self.storage = Storage::of(self.browser.path());
    }

    /// Poll for attached or removed devices, rebuilding the sidebar on a
    /// change. Returns whether anything changed, so the loop can repaint.
    ///
    /// Driven from the event loop rather than from `paint`: painting is the
    /// consequence of a change, and a poll inside it would read the mount
    /// table on every frame of a scroll.
    pub fn poll_volumes(&mut self) -> bool {
        if !self.volumes.poll_signalled().needs_redraw() {
            return false;
        }
        self.rebuild_sidebar();
        // The open directory may have just been unmounted, and the footer
        // still names it.
        self.refresh_storage();
        true
    }

    /// Flush pending writes on the device the sidebar row `index` names.
    ///
    /// Refuses anything that is not a device row: the action's promise is
    /// "it is now safe to unplug", which is meaningless for `/home` and
    /// alarming for `/`. Returns whether a flush was attempted, so a click
    /// on an ordinary place can fall through to navigation.
    #[allow(dead_code)]
    fn flush_device(&self, index: usize) -> bool {
        let Some(row) = self.sidebar.rows().get(index) else {
            return false;
        };
        let Some(path) = row.path().filter(|_| row.is_device()) else {
            return false;
        };
        if let Err(error) = VolumeFlush::flush(path) {
            eprintln!("files: {error}");
        }
        true
    }

    /// Bookmark (or un-bookmark) the directory currently open, persisting the
    /// change and rebuilding the sidebar so the new entry appears at once.
    ///
    /// A save failure is reported rather than swallowed: silently losing a
    /// deliberate action is what makes an application feel broken.
    pub fn toggle_favorite(&mut self) {
        let path = self.browser.path().to_path_buf();
        self.favorites.toggle(&path);
        if let Err(e) = self.favorites.save() {
            eprintln!("files: could not save favorites: {e}");
        }
        self.rebuild_sidebar();
    }

    /// Show or hide the pinned card.
    pub const fn toggle_sidebar(&mut self) {
        self.sidebar.toggle();
    }

    /// Rebuild the sidebar rows after the favorites list or the attached
    /// devices changed.
    fn rebuild_sidebar(&mut self) {
        let dirs = user_dirs::UserDirs::load();
        let visible = self.sidebar.visible();
        self.sidebar = Sidebar::with_storage(
            &dirs,
            &self.favorites,
            self.volumes.volumes(),
            self.volumes.devices(),
        );
        if !visible {
            self.sidebar.toggle();
        }
    }

    /// The window's resolved geometry.
    ///
    /// Every band the painter draws and every hit-test it performs goes
    /// through here, so the two cannot disagree: a click lands on whatever
    /// was painted under it by construction rather than by two parallel
    /// arithmetic expressions being kept in step by hand.
    fn frame(&self) -> Frame {
        Frame::new(
            self.size.width,
            self.size.height,
            self.sidebar.visible(),
            self.theme.spacing,
            self.theme.card_metrics(),
        )
    }

    /// Whether the user has asked to close the window.
    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// Take any file the user activated, for the caller to open.
    pub const fn take_pending_open(&mut self) -> Option<std::path::PathBuf> {
        self.pending_open.take()
    }

    /// The window title: the directory being shown.
    #[must_use]
    pub fn title(&self) -> String {
        self.browser.path().display().to_string()
    }

    /// Record a new window size.
    pub const fn resize(&mut self, size: PhysicalSize) {
        self.size = size;
    }

    /// Fold an [`Outcome`] into window state.
    fn apply(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Quit => self.quit = true,
            Outcome::Open(path) => self.pending_open = Some(path),
            Outcome::ToggleFavorite => self.toggle_favorite(),
            Outcome::ToggleSidebar => self.toggle_sidebar(),
            // A keyboard navigation may have crossed a mount boundary, so the
            // footer is re-read on any redraw-worthy change. Cheap enough --
            // one `statvfs` per keystroke, not per frame.
            Outcome::Redraw => self.refresh_storage(),
            Outcome::Password(reply) => self.answer_password(reply),
            Outcome::Ignored => {}
        }
    }

    /// Start attaching the device the sidebar cursor names.
    ///
    /// Silent when the row is not an unmounted device: the action is offered
    /// from a sidebar row, and a Mount that acted on whatever happened to be
    /// selected would attach a device the user never pointed at.
    #[allow(dead_code)]
    fn begin_mount(&mut self) {
        let Some(device) = self.mount_candidate.clone() else {
            return;
        };
        if !self.mount.begin(&device) {
            return;
        }
        self.browser
            .set_notice(format!("Mounting {}...", device.display_name()));
    }

    /// Aim the Mount action at the device sidebar row `index` names.
    ///
    /// Returns whether the row was an unmounted device, so a click on an
    /// ordinary place falls through to navigation.
    ///
    /// The device is looked up in the **live scan** rather than rebuilt from
    /// the row: the row carries a label and a `/dev` path for drawing, while
    /// mounting needs the filesystem type and the device numbers. Rebuilding
    /// a device from what the sidebar happens to display would be inventing
    /// facts about hardware.
    #[allow(dead_code)]
    fn aim_mount(&mut self, index: usize) -> bool {
        let Some(source) = self
            .sidebar
            .rows()
            .get(index)
            .and_then(SidebarRow::mountable_source)
        else {
            return false;
        };
        // A stale row is a real case: the scan runs on a timer, so the stick
        // may have been pulled between the last poll and this click. Doing
        // nothing is right -- there is no device to attach.
        let Some(device) = self.volumes.devices().by_source(source).cloned() else {
            return true;
        };
        self.mount_candidate = Some(device);
        true
    }

    /// Aim at, and immediately attach, the device sidebar row `index` names.
    ///
    /// Returns whether the click was consumed.
    #[allow(dead_code)]
    fn mount_from_sidebar(&mut self, index: usize) -> bool {
        if !self.aim_mount(index) {
            return false;
        }
        self.begin_mount();
        true
    }

    /// Hand the user's reply to the waiting conversation.
    ///
    /// A dismissal abandons the mount outright rather than merely answering
    /// `None`: the conversation would otherwise ask again, and a prompt the
    /// user just closed reappearing is indistinguishable from a stuck window.
    fn answer_password(&mut self, reply: Option<String>) {
        let dismissed = reply.is_none();
        self.mount.answer(reply);
        if dismissed {
            self.mount.abandon();
            self.browser.set_notice(String::from("Mounting cancelled"));
        }
    }

    /// Drain whatever the running mount has to say, returning whether the
    /// window must repaint.
    ///
    /// Called once per pass of the event loop. A prompt arriving here is what
    /// opens the modal, so this is the path that must not be skipped -- the
    /// worker is blocked until it runs.
    pub fn poll_mount(&mut self) -> bool {
        let mut changed = false;
        while let Some(progress) = self.mount.poll() {
            changed = true;
            match progress {
                MountProgress::Prompt { prompt, echo } => {
                    self.browser.begin_password(&prompt, echo);
                }
                MountProgress::Notice(message) => {
                    self.browser.set_password_notice(&message);
                }
                MountProgress::Finished(outcome) => self.finish_mount(&outcome),
            }
        }
        changed
    }

    /// Report a finished mount and show the new volume.
    ///
    /// The prompt is closed first and unconditionally: a conversation can end
    /// while its question is still on screen -- the authorization was cached,
    /// or another agent answered -- and a modal left open over a finished
    /// mount holds the keyboard in a mode with nothing to answer.
    fn finish_mount(&mut self, outcome: &MountOutcome) {
        self.browser.end_password();
        self.browser.set_notice(Self::describe_mount(outcome));
        if outcome.is_mounted() {
            // The new volume is not in the mount table this window last read,
            // so the sidebar would not show it until the poll interval
            // elapsed -- seconds after the user typed their password.
            self.volumes.poll();
            self.rebuild_sidebar();
            self.refresh_storage();
        }
    }

    /// The status line for a finished mount.
    fn describe_mount(outcome: &MountOutcome) -> String {
        outcome.message().unwrap_or_else(|| match outcome {
            MountOutcome::Cancelled => String::from("Mounting cancelled"),
            _ => String::from("Device mounted"),
        })
    }

    /// Which card control is under `at`, if any.
    fn toolbar_hit(&self, at: Position) -> Option<Target> {
        self.toolbar().and_then(|bar| bar.hit(at.x, at.y))
    }

    /// What the pointer at `at` is over: a control, or a tile.
    ///
    /// The regions are tested in the same order the press handler tests them,
    /// and only one can win, so the highlight cannot appear in two places at
    /// once -- and the thing that lights up is the thing a click would hit.
    fn spot_at(&self, at: Position) -> Option<Spot> {
        if let Some(spot) = self.open_tile_spot(at) {
            return Some(spot);
        }
        if let Some(target) = self.toolbar_hit(at) {
            return Some(Spot::Control(target));
        }
        let Some(placed) = self.placed_at(at) else {
            return self.more_at(at).then_some(Spot::More);
        };
        let row = placed.row;
        Some(
            self.action_at(&placed, row, at)
                .map_or(Spot::Item(Pane::Tiles, row), |action| {
                    Spot::Action(row, action)
                }),
        )
    }

    /// What `at` is over on the card of the tile with the tray open, which
    /// reaches past its cell.
    fn open_tile_spot(&self, at: Position) -> Option<Spot> {
        let (row, _) = self.open_tray().filter(|_| self.on_open_card(at))?;
        let placed = self.placed_item(row)?;
        Some(
            self.action_at(&placed, row, at)
                .map_or(Spot::Item(Pane::Tiles, row), |action| {
                    Spot::Action(row, action)
                }),
        )
    }

    /// The tile whose cell holds `at`.
    fn placed_at(&self, at: Position) -> Option<PlacedTile> {
        let point = Point::new(at.x, at.y);
        self.tile_layout()
            .into_iter()
            .find(|placed| placed.rect.contains(point))
    }

    /// The action button of the tile at `row` that `at` is over, where the
    /// button is *drawn* -- risen with its tile and settling as it appears.
    fn action_at(&self, placed: &PlacedTile, row: usize, at: Position) -> Option<TileAction> {
        if self.browser.is_renaming() {
            return None;
        }
        let spacing = self.theme.spacing;
        let key = TileKey::Item(row);
        let raise = self.motion.raise(key);
        let level = self.motion.tray(row);
        let tray = self
            .open_tray()
            .filter(|&(open, _)| open == row)
            .map(|(_, tray)| tray);
        let slide = tray.as_ref().map_or(0, |tray| tray.slide_at(level));
        let shift = TilePainter::button_shift(spacing, raise, self.motion.reveal(key)) - slide;
        let geometry = TileGeometry::of(placed.rect, spacing, self.type_scale.tile_lines());
        geometry
            .bar
            .hit(Point::new(at.x, at.y - shift))
            .or_else(|| {
                let lift = TilePainter::lift(spacing, raise);
                tray?.hit(Point::new(at.x, at.y), lift, level)
            })
    }

    /// The action a press at `at` would run: a button the pointer is on,
    /// once the tile has shown it clearly enough to be pressed.
    fn live_action_at(&self, at: Position) -> Option<(usize, TileAction)> {
        let (placed, row) =
            if let Some((row, _)) = self.open_tray().filter(|_| self.on_open_card(at)) {
                (self.placed_item(row)?, row)
            } else {
                let placed = self.placed_at(at)?;
                let row = placed.row;
                (placed, row)
            };
        let action = self.action_at(&placed, row, at)?;
        let live = self.motion.buttons_are_live(row)
            && (!TileAction::MENU.contains(&action) || self.motion.tray_is_live(row));
        live.then_some((row, action))
    }

    /// Do what a tile button promises to the entry at `row`.
    fn run_action(&mut self, row: usize, action: TileAction) {
        let outcome = match action {
            TileAction::CopyPath => {
                let Some(path) = self.browser.path_of(row) else {
                    return;
                };
                self.pending_copy = Some(path.display().to_string());
                Outcome::Redraw
            }
            TileAction::Copy => self.browser.clip_entry(row, false),
            TileAction::Cut => self.browser.clip_entry(row, true),
            TileAction::Rename => self.browser.rename_entry(row),
            TileAction::MoveTo => self.browser.move_entry(row),
            TileAction::Open => self.browser.activate_entry(row),
            TileAction::Duplicate => self.browser.duplicate_entry(row),
            TileAction::CopyName => {
                let Some(name) = self.browser.name_of(row) else {
                    return;
                };
                self.pending_copy = Some(name);
                Outcome::Redraw
            }
            TileAction::Trash => self.browser.trash_entry(row),
        };
        self.tray = None;
        self.apply(outcome);
    }

    fn home_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }

    /// Whether Home would go anywhere: `$HOME` is set and is not already open.
    fn can_go_home(&self) -> bool {
        Self::home_dir().is_some_and(|home| home != self.browser.path())
    }

    fn go_home(&mut self) -> Outcome {
        match Self::home_dir() {
            Some(home) => self.browser.go_to(&home),
            None => Outcome::Ignored,
        }
    }

    /// Act on a click on toolbar control `target`.
    fn activate_control(&mut self, target: Target) {
        let outcome = match target {
            Target::Back => self.browser.go_back(),
            Target::Forward => self.browser.go_forward(),
            // The same action the keyboard's Left and Backspace already
            // reach, so the button and the key cannot diverge.
            Target::Up => self.step_out(),
            Target::Home => self.go_home(),
            Target::NewFolder => self.browser.create_folder(),
            Target::ToggleHidden => self.browser.act(crate::keymap::Action::ToggleHidden),
            Target::FilterBox => self.browser.focus_filter(),
        };
        self.apply(outcome);
        // Every navigation can cross a filesystem boundary, exactly as a
        // sidebar jump can, so the storage footer must be re-read.
        if matches!(
            target,
            Target::Back | Target::Forward | Target::Up | Target::Home
        ) {
            self.refresh_storage();
        }
    }

    /// Whether this click, at `now`, continues a double-click gesture.
    fn is_double_click(&mut self, now: u64, at: Position) -> bool {
        let doubled = self.last_click.is_some_and(|(then, prev)| {
            now.saturating_sub(then) <= DOUBLE_CLICK_MS
                && (prev.x - at.x).abs() <= DOUBLE_CLICK_SLOP
                && (prev.y - at.y).abs() <= DOUBLE_CLICK_SLOP
        });
        // A completed double-click resets the timer: without this, a third
        // click would pair with the second and open the file again.
        self.last_click = if doubled { None } else { Some((now, at)) };
        doubled
    }

    /// Milliseconds from a monotonic clock, for click timing.
    ///
    /// Monotonic rather than wall-clock so an NTP step cannot make two
    /// clicks appear an hour apart -- the same reasoning as the key-repeat
    /// clock in `platform`.
    fn now_ms() -> u64 {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        let start = START.get_or_init(std::time::Instant::now);
        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Draw the pinned card: its glass surface and outline, then every piece
    /// of chrome it holds. Nothing is drawn when the user has hidden it.
    fn paint_card(&mut self, canvas: &mut Canvas<'_>) {
        let Some(card) = self.card() else {
            return;
        };
        self.card_surface(&card).paint(canvas, &self.theme);
        self.paint_nav_buttons(canvas);
        self.paint_hidden_toggle(canvas);
        self.paint_breadcrumb(canvas);
        self.paint_filter_control(canvas);
        self.paint_storage(canvas);
    }

    fn card_surface(&self, card: &PinnedCard) -> CardSurface {
        CardSurface::new(card.bounds(), self.storage.as_ref().map(Storage::usage))
    }

    /// The colours behind a control occupying `band`: the used-space tone
    /// below the fill level, the plain surface above it.
    fn backdrop(&self, band: Rect) -> Backdrop {
        self.card().map_or_else(
            || Backdrop::plain(&self.theme),
            |card| self.card_surface(&card).backdrop_for(band, &self.theme),
        )
    }

    /// The card's control geometry, shared by the painter and the pointer
    /// handler so the clickable region cannot drift from the drawn one.
    fn toolbar(&self) -> Option<Toolbar> {
        let card = self.card()?;
        Some(Toolbar::new(
            card,
            self.theme.spacing,
            self.theme.control_height(),
        ))
    }

    /// Draw the back, forward and up buttons at the toolbar's left.
    ///
    /// Each is live only when it would actually go somewhere, and is drawn
    /// muted when it would not -- a disabled control that looks enabled
    /// teaches the user the application is broken, and one that looks
    /// disabled but works is just as confusing. Hover is drawn only on a live
    /// button, for the same reason.
    ///
    /// Up is dead at the filesystem root, which is the same rule
    /// `Browser::go_up` already applies by refusing to move: the button
    /// states what the action would do rather than inventing its own
    /// condition.
    fn paint_nav_buttons(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let Some(bar) = self.toolbar() else {
            return;
        };
        for (rect, glyph, enabled, target) in [
            (
                bar.back(),
                "\u{2039}",
                self.browser.can_go_back(),
                Target::Back,
            ),
            (
                bar.forward(),
                "\u{203a}",
                self.browser.can_go_forward(),
                Target::Forward,
            ),
            (bar.up(), "\u{2191}", self.can_step_out(), Target::Up),
            (bar.home(), "~", self.can_go_home(), Target::Home),
            (bar.new_folder(), "+", true, Target::NewFolder),
        ] {
            // Dropped for want of room: nothing to draw, and `Toolbar::hit`
            // will not report it either.
            let Some(rect) = rect else { continue };
            // Only a button that would do something lights up under the
            // pointer, so the highlight is a promise rather than decoration.
            let hovered = enabled && self.hover.control() == Some(target);
            let backdrop = self.backdrop(rect);
            let bg = if hovered {
                canvas.fill_round_rect(rect, backdrop.hover, theme.spacing.radius_small());
                backdrop.hover
            } else {
                backdrop.surface
            };
            let color = if enabled { theme.text } else { theme.disabled };
            let tier = &mut self.type_scale.title;
            let text_w = tier.measure(glyph);
            let text_y = rect.y + (rect.h - tier.line_height()) / 2;
            tier.draw_text(
                canvas,
                glyph,
                rect.x + (rect.w - text_w) / 2,
                text_y,
                color,
                bg,
            );
        }
    }

    /// Draw the dotfile visibility toggle, beside the navigation chevrons.
    ///
    /// Drawn in the accent wash while hidden files are shown. That is the
    /// whole job of this control: dotfiles being listed is a mode the user can forget they are
    /// in, and an unlit button gives them nothing to notice.
    ///
    /// The glyph changes with the state as well as the fill, because a colour
    /// alone is not an answer for anyone who cannot distinguish it. A filled
    /// disc means "showing everything" and a hollow ring means "showing some",
    /// which is the same full/empty metaphor the storage meter uses.
    ///
    /// `\u{25cf}` rather than `\u{25c9}`: the fisheye's inner dot is a
    /// hairline at label size, so the pair differed by a couple of pixels
    /// that vanished into the accent wash behind them.
    fn paint_hidden_toggle(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let Some(rect) = self.toolbar().and_then(|bar| bar.hidden_toggle()) else {
            return;
        };
        let on = self.browser.shows_hidden();
        let hovered = self.hover.control() == Some(Target::ToggleHidden);
        let radius = theme.spacing.radius_small();
        let backdrop = self.backdrop(rect);
        let bg = if on {
            canvas.fill_round_rect(rect, backdrop.active, radius);
            backdrop.active
        } else if hovered {
            canvas.fill_round_rect(rect, backdrop.hover, radius);
            backdrop.hover
        } else {
            backdrop.surface
        };
        let color = if on { theme.accent } else { theme.text };
        let glyph = if on { "\u{25cf}" } else { "\u{25cb}" };
        let tier = &mut self.type_scale.label;
        let text_w = tier.measure(glyph);
        let text_y = rect.y + (rect.h - tier.line_height()) / 2;
        tier.draw_text(
            canvas,
            glyph,
            rect.x + (rect.w - text_w) / 2,
            text_y,
            color,
            bg,
        );
    }

    /// Draw the breadcrumb path, most-significant segment first.
    ///
    /// The current segment is drawn in full-strength text and the ancestors
    /// muted, so the eye lands on where you *are* rather than on the path you
    /// took to get there.
    fn paint_breadcrumb(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let crumbs = self.crumbs();
        let Some(bar) = self.toolbar().map(|bar| bar.breadcrumbs()) else {
            return;
        };
        let mut x = bar.x;
        let surface = self.backdrop(bar).surface;

        let (shown, elided) = crumbs.tail(self.crumb_budget(bar.w));
        if elided {
            let label = &mut self.type_scale.label;
            let y = label.baseline_y(bar.h) + bar.y;
            x = label.draw_text(canvas, Breadcrumb::ELLIPSIS, x, y, theme.muted, surface);
            x += theme.spacing.xs();
        }

        for (i, crumb) in shown.iter().enumerate() {
            // A separator between segments, but never before the first: the
            // root crumb already reads as "/".
            if i > 0 || elided {
                let label = &mut self.type_scale.label;
                let y = label.baseline_y(bar.h) + bar.y;
                x = label.draw_text(canvas, "/", x, y, theme.muted, surface);
                x += theme.spacing.xs();
            }
            let (color, tier) = if crumb.current {
                (theme.text, &mut self.type_scale.title)
            } else {
                (theme.muted, &mut self.type_scale.body)
            };
            let y = tier.baseline_y(bar.h) + bar.y;
            x = tier.draw_text(canvas, &crumb.label, x, y, color, surface);
            x += theme.spacing.xs();
        }
    }

    /// How many path segments fit in `width` pixels.
    ///
    /// Estimated from the body tier's average advance rather than measured
    /// per crumb: an exact fit would need the widths of every segment before
    /// choosing how many to drop, and at toolbar scale the estimate is off by
    /// at most one segment while being far simpler to reason about.
    fn crumb_budget(&mut self, width: i32) -> usize {
        if width <= 0 {
            return 1;
        }
        // A typical segment plus its separator, in average advances.
        let advance = self.type_scale.body.measure("n").max(1);
        let per_crumb = advance * 10;
        usize::try_from(width / per_crumb.max(1))
            .unwrap_or(1)
            .max(1)
    }

    /// The breadcrumb model for the open directory.
    fn crumbs(&self) -> Breadcrumb {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        Breadcrumb::new(self.browser.path(), home.as_deref())
    }

    /// Draw the filter control at the toolbar's right end.
    ///
    /// Per the guide it is a quiet bordered box that strengthens its border
    /// and widens when focused; here "focused" means the browser is filtering,
    /// which is the only way the control can be active.
    fn paint_filter_control(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let filtering = self.browser.is_filtering();
        // Guard against a column too narrow to hold the control at all --
        // drawing it at a negative x would wrap around the canvas.
        let Some(rect) = self.toolbar().and_then(|bar| bar.filter()) else {
            return;
        };
        let (x, y, height) = (rect.x, rect.y, rect.h);
        let radius = theme.spacing.radius();
        let well = self.backdrop(rect).input;
        canvas.fill_round_rect(rect, well, radius);
        canvas.draw_round_rect(
            rect,
            if filtering {
                theme.focus_ring
            } else {
                theme.control_border
            },
            radius,
        );

        // Text clears the rounded corner rather than just the border: at the
        // box's vertical centre the arc has closed, but starting text at the
        // plain gap step still visually crowds the curve.
        let inner_x = x + theme.spacing.m();
        let detail = &mut self.type_scale.detail;
        let text_y = y + (height - detail.line_height()) / 2;
        if filtering {
            let query = self.browser.filter().text().to_owned();
            let before = self.browser.filter().before_caret().to_owned();
            let detail = &mut self.type_scale.detail;
            detail.draw_text(canvas, &query, inner_x, text_y, theme.text, well);
            // Measured, not counted: with a proportional face the caret's
            // pixel offset is the width of the text before it.
            let caret_x = inner_x + detail.measure(&before);
            canvas.fill_rect(
                Rect::new(caret_x, text_y, theme.spacing.rule(), detail.line_height()),
                theme.caret,
            );
        }
    }

    /// Where the footer text begins: the card's padding plus the row's
    /// internal padding.
    fn sidebar_text_x(theme: &Theme) -> i32 {
        theme.spacing.sidebar_pad() + theme.spacing.sidebar_row_pad()
    }

    /// What the footer says: a current error or operation result, which
    /// explains why the listing is not what the user expected, otherwise the
    /// used and total space.
    fn footer_line(&self) -> Option<(String, Color)> {
        if let Some(error) = self.browser.error() {
            return Some((error.message(), self.theme.error));
        }
        if let Some(notice) = self.browser.notice() {
            return Some((notice.to_owned(), self.theme.muted));
        }
        self.storage
            .as_ref()
            .map(|storage| (storage.usage().figures(), self.theme.muted))
    }

    /// Draw the footer as one small line aligned with the place labels. The
    /// disk's fullness itself is the card's background fill.
    fn paint_storage(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let Some(area) = self.card().and_then(|card| card.storage()) else {
            return;
        };
        let Some((line, color)) = self.footer_line() else {
            return;
        };
        let surface = self.backdrop(area).surface;
        let detail = &mut self.type_scale.detail;
        let y = area.y + (area.h - detail.line_height()) / 2;
        detail.draw_text(
            canvas,
            &line,
            area.x + Self::sidebar_text_x(&theme),
            y,
            color,
            surface,
        );
    }

    /// How many characters of `text` fit in `width` pixels for a given tier.
    ///
    /// The bitmap font was fixed-width, so the old code could divide by a
    /// constant glyph width. A real face cannot: this measures the tier's own
    /// average advance instead of assuming one.
    #[allow(dead_code)]
    fn columns_for(tier: &mut statusbar::TextRenderer, width: i32) -> usize {
        if width <= 0 {
            return 0;
        }
        // "n" is a reasonable stand-in for average advance in a proportional
        // face and is exact in a monospaced one.
        let advance = tier.measure("n").max(1);
        usize::try_from(width / advance).unwrap_or(0)
    }

    /// The open rename box's contents, or `None` when no rename is active.
    fn painted_rename(&self) -> Option<PaintedRename> {
        let rename = self.browser.rename();
        let input = rename.input()?;
        Some(PaintedRename {
            row: rename.row()?,
            text: input.text().to_owned(),
            before_caret: input.before_caret().to_owned(),
            valid: rename.is_valid(),
        })
    }

    /// Draw the password prompt over the whole window, if one is open.
    ///
    /// The style is assembled here rather than held on the panel: the theme
    /// can be re-read at any time, and a cached copy of its colours would
    /// paint the modal in the previous palette.
    fn paint_password(&mut self, canvas: &mut Canvas<'_>) {
        if !self.browser.password().is_open() {
            return;
        }
        let bounds = self.frame().size();
        let style = self.panel_style();
        // Split so the panel borrows the modal while the renderers are
        // borrowed from `type_scale` -- both live on `self`.
        let Self {
            browser,
            type_scale,
            ..
        } = self;
        let mut text = PanelText {
            title: &mut type_scale.title,
            body: &mut type_scale.body,
            detail: &mut type_scale.detail,
        };
        let Some(panel) = PasswordPanel::lay_out(bounds, style, &text) else {
            return;
        };
        panel.paint(canvas, browser.password(), style, &mut text, bounds);
    }

    /// The modal's colours, drawn from the theme's existing roles.
    ///
    /// Reused rather than given new config keys, for the same reason the
    /// sidebar's are: a re-themed palette must carry the modal with it
    /// instead of stranding a set of hand-picked colours.
    const fn panel_style(&self) -> PanelStyle {
        let theme = self.theme;
        PanelStyle {
            spacing: theme.spacing,
            surface: theme.panel_surface,
            border: theme.panel_border,
            text: theme.text,
            muted: theme.muted,
            notice: theme.error,
            field: theme.filter_bg,
            focus_ring: theme.focus_ring,
            caret: theme.caret,
            scrim: theme.panel_scrim,
        }
    }

    /// Draw the floating "N items" badge while a drag is in flight.
    ///
    /// Uses the accent surface rather than a row fill: the badge belongs to
    /// the gesture, not to the listing, and reusing a row colour would make
    /// it read as another row.
    fn paint_drag_badge(&mut self, canvas: &mut Canvas<'_>) {
        if !self.drag_badge.is_active() {
            return;
        }
        let theme = self.theme;
        let bounds = self.frame().field();
        self.drag_badge.paint(
            canvas,
            &mut self.type_scale.detail,
            bounds,
            BadgeStyle {
                spacing: theme.spacing,
                fill: theme.accent_surface,
                border: theme.accent,
                fg: theme.text,
            },
        );
    }
}

impl Painter for FilesWindow {
    fn background(&self) -> Color {
        self.theme.background
    }

    fn paint(&mut self, canvas: &mut Canvas<'_>, size: PhysicalSize) {
        // The painter is the authority on size only for drawing; the browser
        // is resized through the window's own resize path, which the event
        // loop drives from WindowEvent::Resized.
        self.size = size;
        // The footer's figures are read once per directory, not per frame; the
        // first paint is where the initial read happens, since `new` is const.
        if self.storage.is_none() {
            self.refresh_storage();
        }
        canvas.clear(self.theme.background);
        self.sync_tiles();
        self.paint_tiles(canvas);
        self.paint_card(canvas);
        self.paint_move_prompt(canvas);
        // Last, and over everything: the badge is cargo the user is holding
        // above the window, so anything it covers is by definition behind it.
        self.paint_drag_badge(canvas);
        // Later still: the modal covers the badge too. While it is up nothing
        // behind it is usable, so nothing behind it may be drawn on top.
        self.paint_password(canvas);
    }

    fn on_key(&mut self, press: KeyPress) {
        self.sync_tiles();
        if self.tray.take().is_some() && press.key == Key::Escape {
            return;
        }
        let outcome = match self.browser.translate(press) {
            Some(crate::keymap::Action::GoUp) => self.step_out(),
            Some(crate::keymap::Action::List(command)) if SpiralCursor::steers(command) => {
                self.steer_tiles(command)
            }
            _ => self.browser.key(press),
        };
        self.apply(outcome);
        self.sync_tiles();
    }

    fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn on_pointer(&mut self, event: PointerEventKind) {
        // A modal that lets a click through is not modal. Every gesture is
        // swallowed while the prompt is up: a press would select a file, a
        // motion would light a row the user cannot reach, and a scroll would
        // move a list behind a box they are typing a password into.
        //
        // Swallowed rather than routed to the panel: the field has no caret
        // to place and no text to select, so there is nothing on the modal a
        // click could usefully do.
        if self.browser.password().is_open() {
            return;
        }
        self.sync_tiles();
        match event {
            PointerEventKind::Press {
                button: PointerButton::Left,
                at,
                modifiers,
            } => self.press_left(
                at,
                Mods {
                    ctrl: modifiers.ctrl(),
                    shift: modifiers.shift(),
                },
            ),
            PointerEventKind::Press {
                button: PointerButton::Right,
                at,
                ..
            } => self.press_right(at),
            PointerEventKind::Release { at, .. } => {
                self.drag_arm.cancel();
                self.drag_row = None;
                self.release_action(at);
            }
            PointerEventKind::Motion(at) => {
                // A drag is armed only while a row press hasn't yet crossed
                // the threshold; once it fires this is a one-shot -- the
                // compositor takes over pointer delivery for the rest of the
                // gesture the moment `start_drag` is queued.
                if let Some(row) = self.drag_row
                    && self.drag_arm.motion_exceeds_threshold((at.x, at.y))
                {
                    let paths = self.browser.drag_paths(row);
                    self.drag_badge.begin(paths.len(), (at.x, at.y));
                    self.pending_drag = Some(paths);
                    self.drag_row = None;
                    return;
                }
                // The changed-flag is deliberately discarded: `Toplevel`
                // marks the surface dirty after *every* pointer event it
                // routes, so there is no frame here for us to save. `Hover`
                // reports it anyway because that is the honest signal for the
                // state it owns, and the day this window drives its own
                // damage the call site should not have to grow the logic back.
                //
                // The panes are checked in the same order the press handler
                // checks them, and only one can win, so the highlight cannot
                // appear in both at once.
                let spot = self.spot_at(at);
                let _ = self.hover.move_to(spot);
            }
            PointerEventKind::Scroll { lines, at, .. } => {
                self.slide_tiles(lines, at);
            }
            // The pointer left the surface, so the highlight must go with it;
            // otherwise a row stays lit while the pointer is in another window.
            PointerEventKind::Leave => {
                let _ = self.hover.clear();
                self.pressed_action = None;
                self.drag_arm.cancel();
                self.drag_row = None;
            }
            // Enter carries a position, but the Motion event that follows it
            // does the hover work -- handling it here too would only duplicate
            // that. The middle button stays inert.
            PointerEventKind::Enter(_) | PointerEventKind::Press { .. } => {}
        }
    }

    fn on_files_dropped(&mut self, paths: Vec<std::path::PathBuf>, at: (i32, i32)) {
        self.sync_tiles();
        let target = self.browser.drop_dir(self.entry_at(at));
        let outcome = self.browser.accept_drop(&paths, &target);
        self.apply(outcome);
        self.drop_hover_row = None;
        self.drag_badge.end();
    }
}

#[cfg(test)]
impl FilesWindow {
    /// Paint one frame and digest the pixels of the tile field.
    ///
    /// The crop is what makes a tile test mean anything: the status line
    /// reports "1 item cut" against "1 item copied", so a whole-frame
    /// comparison passes on that text alone and proves nothing about the
    /// tiles.
    ///
    /// A digest rather than the pixels themselves, because a failed
    /// `assert_ne!` prints its operands: half a million integers is not a
    /// diagnostic, it is a flooded terminal.
    fn painted_field(&mut self, size: platform::toplevel::PhysicalSize) -> u64 {
        let px = (size.width * size.height).cast_unsigned() as usize;
        let mut buf = vec![0u32; px];
        {
            let mut canvas = platform::draw::Canvas::new(&mut buf, size.width, size.height);
            self.paint(&mut canvas, size);
        }
        let band = self.frame().field();
        let stride = size.width.cast_unsigned() as usize;
        let top = (band.y.max(0).cast_unsigned() as usize * stride).min(px);
        let bottom = ((band.y + band.h).max(0).cast_unsigned() as usize * stride).min(px);
        buf[top..bottom]
            .iter()
            .fold(0xcbf2_9ce4_8422_2325, |h, &p| {
                (h ^ u64::from(p)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FilesWindow, KeyPress, Painter, PointerEventKind, Target, TileAction, TileGeometry, TileKey,
    };
    use crate::hover::{Pane, Spot};
    use platform::SurfaceFormat;
    use platform::draw::Canvas;
    use platform::protocol::PointerButton;
    use platform::toplevel::PhysicalSize;

    /// Paint a whole frame into a translucent canvas and hand back the pixels.
    ///
    /// Shared by the transparency tests below so each states only the property
    /// it is checking.
    fn painted_translucent(bg_alpha: u8) -> (Vec<u32>, PhysicalSize) {
        let size = PhysicalSize {
            width: 900,
            height: 600,
        };
        let cfg = config::Config::default();
        let theme = tiles::theme::Theme::from_config(&cfg);
        let type_scale = tiles::typography::Typography::load(&cfg, 1.0);
        let favorites = crate::favorites::Favorites::default();
        let user_dirs = user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        // The temp dir always exists and is readable, so `open` succeeds
        // without depending on anything in the developer's home.
        let browser = crate::browser::Browser::open(&std::env::temp_dir(), 700, 400, 28)
            .expect("the temp dir must be listable");
        let mut painter = FilesWindow::new(
            browser,
            theme,
            type_scale,
            sidebar,
            favorites,
            size,
            waker::Waker::new().expect("an eventfd"),
        );
        let px = (size.width * size.height).cast_unsigned() as usize;
        let mut buf = vec![0u32; px];
        {
            let mut canvas = Canvas::new(&mut buf, size.width, size.height)
                .with_format(SurfaceFormat::with_bg_alpha(bg_alpha));
            painter.paint(&mut canvas, size);
        }
        (buf, size)
    }

    /// The regression this exists to prevent: a window that negotiated an
    /// `Argb8888` buffer but whose painter fills opaque everywhere renders as
    /// a solid rectangle, and the configured opacity silently does nothing.
    ///
    /// Asserted as a *proportion* of the surface rather than "some pixel is
    /// translucent", because a single stray see-through pixel would satisfy
    /// the weaker claim while the window still looked opaque.
    #[test]
    fn a_translucent_browser_actually_lets_the_desktop_through() {
        let (buf, _) = painted_translucent(204);
        let translucent = buf.iter().filter(|&&px| (px >> 24) != 255).count();
        // Integer percentage rather than a float ratio: exact, and avoids a
        // lossy usize->f64 cast for what is only ever a reported figure.
        let percent = translucent * 100 / buf.len();
        assert!(
            percent > 50,
            "most of the window should be see-through glass, got {percent}%"
        );
    }

    /// Text and chrome must stay solid over the glass, or the wallpaper shows
    /// through the filenames and the listing becomes unreadable. Some fully
    /// opaque pixels must therefore survive the frame.
    #[test]
    fn the_browsers_text_and_chrome_stay_solid_over_the_glass() {
        let (buf, _) = painted_translucent(204);
        let solid = buf.iter().filter(|&&px| (px >> 24) == 255).count();
        assert!(
            solid > 0,
            "the glyphs and separators must be painted as solid ink"
        );
    }

    /// Every pixel of a translucent frame must be a valid premultiplied ARGB
    /// word (no channel above alpha). A violation is what the compositor
    /// renders as bright fringing or inverted colour.
    #[test]
    fn every_painted_pixel_is_valid_premultiplied_argb() {
        let (buf, size) = painted_translucent(204);
        let stride = size.width.cast_unsigned() as usize;
        for (i, &px) in buf.iter().enumerate() {
            let a = (px >> 24) & 0xff;
            for shift in [16, 8, 0] {
                assert!(
                    (px >> shift) & 0xff <= a,
                    "pixel ({}, {}) is not valid premultiplied: {px:#010x}",
                    i % stride,
                    i / stride,
                );
            }
        }
    }

    /// An opaque browser must be unaffected by all of this: every pixel of the
    /// frame ends up covered, so none may keep the zero-alpha value the buffer
    /// started at.
    #[test]
    fn an_opaque_browser_paints_every_pixel_of_its_frame() {
        let size = PhysicalSize {
            width: 900,
            height: 600,
        };
        let cfg = config::Config::default();
        let theme = tiles::theme::Theme::from_config(&cfg);
        let type_scale = tiles::typography::Typography::load(&cfg, 1.0);
        let favorites = crate::favorites::Favorites::default();
        let user_dirs = user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        let browser = crate::browser::Browser::open(&std::env::temp_dir(), 700, 400, 28)
            .expect("the temp dir must be listable");
        let mut painter = FilesWindow::new(
            browser,
            theme,
            type_scale,
            sidebar,
            favorites,
            size,
            waker::Waker::new().expect("an eventfd"),
        );
        let px = (size.width * size.height).cast_unsigned() as usize;
        // A sentinel the painter can never legitimately produce on an opaque
        // surface, where every packed word has a zero top byte.
        let mut buf = vec![0xdead_beef_u32; px];
        {
            let mut canvas = Canvas::new(&mut buf, size.width, size.height);
            painter.paint(&mut canvas, size);
        }
        assert!(
            buf.iter().all(|&p| p != 0xdead_beef),
            "an opaque frame must cover every pixel"
        );
        assert!(
            buf.iter().all(|&p| (p >> 24) == 0),
            "an opaque surface packs a zero top byte"
        );
    }

    /// Build a painter over a directory holding one subdirectory and one
    /// regular file, so the per-type highlights below have both kinds to
    /// distinguish. Returns the painter and the temp dir, which must stay
    /// alive for the duration of the test.
    ///
    /// `tag` names the directory. It is required rather than derived from the
    /// pid because cargo runs these tests *in parallel threads of one
    /// process*: a shared pid-only name had them racing, one test's cleanup
    /// deleting the tree another was still listing.
    fn painter_over_a_mixed_directory(
        tag: &str,
    ) -> (FilesWindow, std::path::PathBuf, PhysicalSize) {
        let size = PhysicalSize {
            width: 900,
            height: 600,
        };
        // Not `temp_dir()` itself: its contents are whatever the machine
        // happens to have, so "row 0 is a directory" would not be guaranteed.
        let root = std::env::temp_dir().join(format!("files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a_directory")).expect("create a subdirectory");
        std::fs::write(root.join("b_file.txt"), b"x").expect("create a file");
        std::fs::create_dir_all(root.join(".a_hidden_dir")).expect("create a dot directory");
        std::fs::write(root.join(".a_dotfile"), b"x").expect("create a dotfile");

        let cfg = config::Config::default();
        let theme = tiles::theme::Theme::from_config(&cfg);
        let type_scale = tiles::typography::Typography::load(&cfg, 1.0);
        let favorites = crate::favorites::Favorites::default();
        let user_dirs = user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        let browser = crate::browser::Browser::open(&root, 700, 400, 28).expect("listable");
        let mut painter = FilesWindow::new(
            browser,
            theme,
            type_scale,
            sidebar,
            favorites,
            size,
            waker::Waker::new().expect("an eventfd"),
        );
        painter.sync_tiles();
        (painter, root, size)
    }

    /// Paint `painter` into an opaque canvas and return the pixels.
    fn paint_opaque(painter: &mut FilesWindow, size: PhysicalSize) -> Vec<u32> {
        let px = (size.width * size.height).cast_unsigned() as usize;
        let mut buf = vec![0u32; px];
        {
            let mut canvas = Canvas::new(&mut buf, size.width, size.height);
            painter.paint(&mut canvas, size);
        }
        buf
    }

    /// The centre of a toolbar control, as a pointer position.
    fn control_centre(painter: &FilesWindow, target: Target) -> super::Position {
        let bar = painter.toolbar().expect("the card is shown");
        let rect = match target {
            Target::Back => bar.back(),
            Target::Forward => bar.forward(),
            Target::NewFolder => bar.new_folder(),
            Target::ToggleHidden => bar.hidden_toggle(),
            Target::FilterBox => bar.filter(),
            Target::Up => bar.up(),
            Target::Home => bar.home(),
        }
        .expect("the control must be drawn at this window size");
        super::Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2,
        }
    }

    /// Click a toolbar control by its target, through the real pointer path.
    fn click_control(painter: &mut FilesWindow, target: Target) {
        painter.sync_tiles();
        let at = control_centre(painter, target);
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
    }

    /// Polling on a machine with no removable media must report no change,
    /// so the window does not repaint on a timer.
    #[test]
    fn polling_an_unchanged_system_asks_for_no_repaint() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("dev-poll");
        assert!(
            !painter.poll_volumes(),
            "nothing was plugged in, so nothing must repaint"
        );
    }

    /// The toggle must reveal dotfiles *and* dot-directories, through the
    /// real pointer path -- a button wired to nothing is the bug this guards.
    #[test]
    fn clicking_the_hidden_toggle_reveals_and_rehides_dotfiles() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-hidden");
        assert_eq!(
            painter.browser.len(),
            2,
            "dotfiles start hidden, so only the two plain entries list"
        );
        assert!(!painter.browser.shows_hidden());

        click_control(&mut painter, Target::ToggleHidden);
        assert!(painter.browser.shows_hidden(), "the state must flip");
        assert_eq!(
            painter.browser.len(),
            4,
            "both the dotfile and the dot-directory must appear"
        );

        click_control(&mut painter, Target::ToggleHidden);
        assert!(!painter.browser.shows_hidden(), "and flip back");
        assert_eq!(painter.browser.len(), 2, "hiding them again must restore");
    }

    /// The toggle and Ctrl-H drive the same state, so the button reflects a
    /// change made from the keyboard rather than tracking a private flag.
    #[test]
    fn the_keyboard_shortcut_and_the_button_share_one_state() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-hidden-key");
        let outcome = painter.browser.act(crate::keymap::Action::ToggleHidden);
        painter.apply(outcome);
        assert!(painter.browser.shows_hidden());

        click_control(&mut painter, Target::ToggleHidden);
        assert!(
            !painter.browser.shows_hidden(),
            "the button must continue from where the shortcut left off"
        );
    }

    /// A click on the back chevron must actually navigate.
    ///
    /// Driven through `on_pointer` rather than by calling `go_back` directly:
    /// the thing worth testing is the whole path -- hit-test, routing, browser
    /// -- since a button that is wired to nothing is exactly the bug here.
    #[test]
    fn clicking_back_returns_to_the_previous_directory() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("toolbar-back");
        let sub = root.join("a_directory");
        let outcome = painter.browser.go_to(&sub);
        painter.apply(outcome);
        assert_eq!(painter.browser.path(), sub, "we start in the subdirectory");

        click_control(&mut painter, Target::Back);
        assert_eq!(
            painter.browser.path(),
            root,
            "clicking back must return to the parent"
        );

        click_control(&mut painter, Target::Forward);
        assert_eq!(
            painter.browser.path(),
            sub,
            "and clicking forward must return to the child"
        );
    }

    /// The up arrow must actually leave the directory, through the same
    /// whole path a user's click takes.
    #[test]
    fn clicking_up_leaves_the_open_directory_for_its_parent() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("toolbar-up");
        let sub = root.join("a_directory");
        let outcome = painter.browser.go_to(&sub);
        painter.apply(outcome);
        assert_eq!(painter.browser.path(), sub, "we start in the subdirectory");

        click_control(&mut painter, Target::Up);
        assert_eq!(
            painter.browser.path(),
            root,
            "clicking up must land in the parent"
        );
    }

    /// Going up is a step like any other, so back must undo it. This is the
    /// property that would break if the button called `navigate` directly
    /// instead of reusing the keyboard's own action.
    #[test]
    fn back_undoes_a_click_on_the_up_arrow() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("toolbar-up-back");
        let sub = root.join("a_directory");
        let outcome = painter.browser.go_to(&sub);
        painter.apply(outcome);

        click_control(&mut painter, Target::Up);
        click_control(&mut painter, Target::Back);
        assert_eq!(
            painter.browser.path(),
            sub,
            "up must be recorded in the trail like any other navigation"
        );
    }

    /// At the filesystem root there is nowhere above, and the button must say
    /// so rather than looking live and doing nothing.
    #[test]
    fn the_up_arrow_is_dead_at_the_filesystem_root() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-up-root");
        let outcome = painter.browser.go_to(std::path::Path::new("/"));
        painter.apply(outcome);
        assert!(
            !painter.browser.can_go_up(),
            "the root has no parent, so the button must be drawn dead"
        );
    }

    #[test]
    fn clicking_home_opens_the_home_directory_and_back_returns() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("toolbar-home");
        let Some(home) = FilesWindow::home_dir() else {
            return;
        };
        assert!(painter.can_go_home(), "away from home the button is live");

        click_control(&mut painter, Target::Home);
        assert_eq!(painter.browser.path(), home);
        assert!(!painter.can_go_home(), "at home the button is dead");

        click_control(&mut painter, Target::Back);
        assert_eq!(painter.browser.path(), root);
    }

    #[test]
    fn clicking_new_folder_creates_a_folder_and_a_second_click_a_distinct_one() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("toolbar-new-folder");
        click_control(&mut painter, Target::NewFolder);
        assert!(root.join("New folder").is_dir());
        click_control(&mut painter, Target::NewFolder);
        assert!(root.join("New folder (1)").is_dir());
    }

    /// A click on the filter box must focus it, so typing goes into the box
    /// rather than being treated as list shortcuts.
    #[test]
    fn clicking_the_filter_box_focuses_it() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-filter");
        assert!(!painter.browser.is_filtering(), "it starts unfocused");
        click_control(&mut painter, Target::FilterBox);
        assert!(
            painter.browser.is_filtering(),
            "clicking the box must give it focus"
        );
    }

    /// A click on a toolbar control must not fall through to the row beneath.
    #[test]
    fn a_toolbar_click_does_not_reach_the_list() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-no-fallthrough");
        let before = painter.browser.pane().cursor();
        click_control(&mut painter, Target::FilterBox);
        assert_eq!(
            painter.browser.pane().cursor(),
            before,
            "the click must not also move the list cursor"
        );
    }

    /// A dead chevron must look dead, and a live one must look different --
    /// otherwise the enabled state is a lie the user cannot see.
    #[test]
    fn a_live_chevron_is_painted_differently_from_a_dead_one() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("toolbar-enabled");
        assert!(
            !painter.browser.can_go_back(),
            "a fresh browser has no history"
        );
        let dead = paint_opaque(&mut painter, size);

        let outcome = painter.browser.go_to(&root.join("a_directory"));
        painter.apply(outcome);
        assert!(
            painter.browser.can_go_back(),
            "now there is a step behind us"
        );
        let live = paint_opaque(&mut painter, size);
        assert_ne!(
            dead, live,
            "an armed back button must be visibly different from a dead one"
        );
    }

    /// Hovering a live control must light it up; hovering a dead one must not,
    /// so the highlight is a promise that clicking will do something.
    #[test]
    fn only_a_live_chevron_lights_up_under_the_pointer() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("toolbar-hover");

        // Dead: hovering must change nothing at all.
        let cold = paint_opaque(&mut painter, size);
        assert!(painter.hover.move_to(Some(Spot::Control(Target::Back))));
        let hovered_dead = paint_opaque(&mut painter, size);
        assert_eq!(
            cold, hovered_dead,
            "a dead button must not light up and promise an action it will not take"
        );

        // Live: the same hover must now be visible.
        assert!(painter.hover.clear());
        let outcome = painter.browser.go_to(&root.join("a_directory"));
        painter.apply(outcome);
        let resting = paint_opaque(&mut painter, size);
        assert!(painter.hover.move_to(Some(Spot::Control(Target::Back))));
        let hovered_live = paint_opaque(&mut painter, size);
        assert_ne!(
            resting, hovered_live,
            "a live button must acknowledge the pointer"
        );
    }

    /// Moving the pointer over a control must drop a row highlight, and vice
    /// versa -- the two must never be lit at once.
    #[test]
    fn the_pointer_lights_either_a_row_or_a_control_but_never_both() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-exclusive");
        let at = control_centre(&painter, Target::FilterBox);
        painter.on_pointer(PointerEventKind::Motion(at));
        assert_eq!(
            painter.hover.control(),
            Some(Target::FilterBox),
            "the pointer is over the filter box"
        );
        assert_ne!(
            painter.hover.spot(),
            Some(Spot::Item(Pane::Tiles, 0)),
            "and so no row may be lit"
        );
    }

    /// Carry the tile glides forward by `frames` ticks of 16 ms, as the loop
    /// would, and return the time reached.
    fn glide(painter: &mut FilesWindow, from_ms: u64, frames: u64) -> u64 {
        for frame in 1..=frames {
            painter.step_motion(from_ms + frame * 16);
        }
        from_ms + frames * 16
    }

    /// Hovering a row must actually change what gets painted -- by gliding,
    /// not by jumping.
    ///
    /// Compares whole frames rather than probing a pixel: the tile's exact
    /// position depends on the frame geometry, and a test that hardcodes a
    /// coordinate breaks every time the layout is tuned.
    #[test]
    fn hovering_a_row_changes_the_painted_frame_by_gliding() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-frame");
        let arrived = glide(&mut painter, 0, 150);
        assert!(!painter.growth.is_animating(), "the entrance has landed");
        let cold = paint_opaque(&mut painter, size);

        assert!(
            painter.hover.move_to(Some(Spot::Item(Pane::Tiles, 0))),
            "row 0 becomes hovered"
        );
        painter.step_motion(arrived + 1000);
        assert_eq!(
            paint_opaque(&mut painter, size),
            cold,
            "the first frame of a glide has not moved yet"
        );
        let started = glide(&mut painter, arrived + 1000, 3);
        let part_way = paint_opaque(&mut painter, size);
        assert_ne!(cold, part_way, "a few frames in, the tile is moving");
        let settled = glide(&mut painter, started, 20);
        let hot = paint_opaque(&mut painter, size);
        assert_ne!(part_way, hot, "and it keeps going to the end");
        assert!(
            !painter.motion.is_animating(),
            "then it stops asking for frames"
        );

        assert!(painter.hover.clear(), "the hover is dropped");
        painter.step_motion(settled);
        let end = glide(&mut painter, settled, 20);
        assert_eq!(
            paint_opaque(&mut painter, size),
            cold,
            "leaving restores the frame exactly once the glide has run, at {end}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tiles_grow_out_of_the_spiral_instead_of_snapping_in() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("grow-in");
        let before = painter.step_motion(5000);
        assert!(before, "the entrance asks for frames");
        assert!(painter.motion_wait_ms().is_some());
        let start = paint_opaque(&mut painter, size);
        let started = glide(&mut painter, 5000, 4);
        let part_way = paint_opaque(&mut painter, size);
        assert!(start != part_way, "tiles are appearing");
        glide(&mut painter, started, 150);
        let landed = paint_opaque(&mut painter, size);
        assert!(part_way != landed, "and they keep growing to full size");
        assert_eq!(painter.motion_wait_ms(), None, "then the loop may sleep");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hovering_the_home_button_changes_the_painted_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-home");
        if !painter.can_go_home() {
            return;
        }
        let cold = paint_opaque(&mut painter, size);
        assert!(painter.hover.move_to(Some(Spot::Control(Target::Home))));
        let hot = paint_opaque(&mut painter, size);
        assert_ne!(cold, hot, "a live Home button must light under the pointer");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The sidebar hover fill has to be visible against the sidebar's *own*
    /// background, and distinct from the active fill -- otherwise hovering
    /// looks like a second active place.
    #[test]
    fn the_sidebar_hover_is_visible_and_not_mistakable_for_active() {
        let (painter, root, _) = painter_over_a_mixed_directory("hover-sidebar-theme");
        let theme = &painter.theme;
        assert_ne!(
            theme.hover, theme.sidebar_bg,
            "an invisible hover is not a hover"
        );
        assert_ne!(
            theme.hover, theme.sidebar_active,
            "hover must not look like the open directory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory and a file must not get the same selection fill, and each
    /// must differ from the plain background or it is invisible.
    #[test]
    fn a_directory_and_a_file_select_differently() {
        let (painter, root, _) = painter_over_a_mixed_directory("select-theme");
        let theme = &painter.theme;
        assert_ne!(
            theme.selection_dir, theme.selection_file,
            "selection must distinguish a directory from a file"
        );
        for fill in [theme.selection_dir, theme.selection_file] {
            assert_ne!(fill, theme.background, "a highlight must be visible");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn selecting_a_tile_changes_the_painted_frame_once_it_has_glided() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("select-paint");
        painter.sync_tiles();
        let before = paint_opaque(&mut painter, size);
        painter.browser.pick(1, widget::Mods::none());
        painter.step_motion(70_000);
        glide(&mut painter, 70_000, 20);
        let after = paint_opaque(&mut painter, size);
        assert!(before != after, "a selected tile must look selected");
        assert!(!painter.motion.is_animating(), "and the glide settles");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A width narrower than a single glyph yields no columns rather than a
    /// huge one: a window dragged narrower than its own padding produces a
    /// negative width, and a sign-losing cast would turn that into two
    /// billion columns.
    #[test]
    fn a_degenerate_width_yields_no_columns() {
        let mut tier = statusbar::TextRenderer::load(None, 14.0);
        assert_eq!(FilesWindow::columns_for(&mut tier, 0), 0);
        assert_eq!(
            FilesWindow::columns_for(&mut tier, -100),
            0,
            "a negative width is no columns"
        );
        assert!(
            FilesWindow::columns_for(&mut tier, 800) > 0,
            "a real width must fit some text"
        );
    }

    fn left_press(painter: &mut FilesWindow, at: super::Position) {
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
    }

    /// A plain click -- press then release with no motion in between -- must
    /// never queue a drag. This is the headline regression a `DragArm`
    /// threshold exists to prevent.
    #[test]
    fn a_plain_click_does_not_start_a_drag() {
        let (mut painter, _root, _size) = tiled("drag-plain-click");
        let at = tile_centre(&painter, "b_file.txt");
        left_press(&mut painter, at);
        painter.on_pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
        assert!(painter.take_pending_drag().is_none());
    }

    /// Moving the pointer past the drag threshold after pressing on a tile
    /// must queue that entry's path as a pending drag.
    #[test]
    fn dragging_past_the_threshold_queues_the_entry_for_a_drag() {
        let (mut painter, _root, _size) = tiled("drag-threshold");
        let at = tile_centre(&painter, "b_file.txt");
        left_press(&mut painter, at);
        assert!(
            painter.take_pending_drag().is_none(),
            "a bare press must not yet be a drag"
        );
        let moved = super::Position {
            x: at.x + 20,
            y: at.y,
        };
        painter.on_pointer(PointerEventKind::Motion(moved));
        let dragged = painter
            .take_pending_drag()
            .expect("motion past the threshold must queue a drag");
        assert_eq!(dragged.len(), 1);
    }

    /// A press on empty field space (no tile under the pointer) must never
    /// arm a drag, no matter how far the pointer then moves. An empty folder
    /// is the one field with no tiles in it.
    #[test]
    fn pressing_empty_space_never_arms_a_drag() {
        let (mut painter, root) = crowded("drag-empty-space", 0);
        let field = painter.frame().field();
        let at = super::Position {
            x: field.x + field.w / 2,
            y: field.y + field.h / 2,
        };
        assert!(
            painter
                .tile_at(platform::draw::Point::new(at.x, at.y))
                .is_none()
        );
        left_press(&mut painter, at);
        let moved = super::Position {
            x: at.x - 50,
            y: at.y,
        };
        painter.on_pointer(PointerEventKind::Motion(moved));
        assert!(painter.take_pending_drag().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Dropping files onto a folder tile must move them there and refresh
    /// the listing -- the full path from `Painter::on_files_dropped` through
    /// to a real filesystem mutation.
    #[test]
    fn dropping_files_onto_a_folder_tile_moves_them_there() {
        let (mut painter, root, _size) = tiled("drag-drop-onto-dir");
        let file = root.join("b_file.txt");
        let folder = root.join("a_directory");
        let at = tile_centre(&painter, "a_directory");
        painter.on_files_dropped(vec![file.clone()], (at.x, at.y));
        assert!(!file.exists(), "the file must have moved out of the root");
        assert!(
            folder.join("b_file.txt").exists(),
            "the file must have landed inside the dropped-on directory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An open rename box must actually reach the framebuffer -- a model
    /// that renames correctly but paints nothing is invisible to the user.
    #[test]
    fn an_open_rename_box_changes_the_painted_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("rename-paint");
        let resting = paint_opaque(&mut painter, size);

        painter
            .browser
            .act(crate::keymap::Action::List(widget::ListCommand::Move(
                widget::Direction::Down,
            )));
        painter.browser.act(crate::keymap::Action::BeginRename);
        assert!(painter.browser.is_renaming(), "F2 must open the box");
        let editing = paint_opaque(&mut painter, size);

        assert_ne!(
            resting, editing,
            "an open rename box must be visible on screen"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An illegal name is refused silently by the model, so the outline is
    /// the only feedback the user gets. It has to differ from the valid one.
    #[test]
    fn an_illegal_rename_paints_differently_from_a_valid_one() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("rename-invalid");
        painter.browser.act(crate::keymap::Action::BeginRename);
        let valid = paint_opaque(&mut painter, size);

        painter.browser.act(crate::keymap::Action::Edit(
            crate::keymap::EditAction::KillToStart,
        ));
        for c in "a/b".chars() {
            painter.browser.act(crate::keymap::Action::Insert(c));
        }
        let invalid = paint_opaque(&mut painter, size);

        assert_ne!(
            valid, invalid,
            "an unusable name must be visibly marked, since Enter does nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ctrl-X gives no other feedback, so the dimmed name is how the user
    /// knows what their cut actually took.
    ///
    /// The cursor is placed first: with no cursor and no selection there is
    /// nothing to cut, and the clipboard correctly stays empty.
    #[test]
    fn a_cut_entry_is_painted_dimmed() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("cut-paint");
        painter
            .browser
            .act(crate::keymap::Action::List(widget::ListCommand::Move(
                widget::Direction::Down,
            )));
        let resting = painter.painted_field(size);

        painter.browser.act(crate::keymap::Action::Cut);
        let after_cut = painter.painted_field(size);

        assert_ne!(
            resting, after_cut,
            "a pending cut must be visible in the listing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A copy is not going anywhere, so dimming it would promise a move that
    /// never comes.
    #[test]
    fn a_copied_entry_is_not_dimmed() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("copy-paint");
        painter
            .browser
            .act(crate::keymap::Action::List(widget::ListCommand::Move(
                widget::Direction::Down,
            )));
        painter.browser.act(crate::keymap::Action::Copy);
        let after_copy = painter.painted_field(size);

        painter.browser.act(crate::keymap::Action::Cut);
        let after_cut = painter.painted_field(size);

        assert_ne!(
            after_copy, after_cut,
            "only a cut may dim its entries, never a copy"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The prompt must actually reach the framebuffer. A modal that models
    /// the conversation correctly and paints nothing leaves the user staring
    /// at a window that has silently stopped accepting input.
    #[test]
    fn an_open_password_prompt_changes_the_painted_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("password-paint");
        let resting = paint_opaque(&mut painter, size);

        painter
            .browser
            .begin_password("Authentication is required to mount KINGSTON", false);
        let prompting = paint_opaque(&mut painter, size);

        assert_ne!(resting, prompting, "the modal must be visible on screen");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The scrim must genuinely *darken* the window, not merely change it.
    ///
    /// The shipped bug this pins: the scrim was painted in the window's own
    /// background colour, and blending a colour over itself is a no-op. The
    /// list stayed exactly as bright while the darker chrome bands were
    /// *lightened* -- the opposite of dimming. Asserting only that pixels
    /// changed passed on precisely that, so this measures brightness on both
    /// panes and requires every sampled pixel to lose some.
    #[test]
    fn the_prompt_darkens_both_panes_without_hiding_them() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("password-scrim");
        let resting = paint_opaque(&mut painter, size);
        painter.browser.begin_password("Password: ", false);
        let prompting = paint_opaque(&mut painter, size);

        // The real luma, rather than the coefficients restated here, so this
        // cannot drift from the brightness rule the theme is checked against.
        let luma = |px: u32| {
            let channel = |shift: u32| u8::try_from((px >> shift) & 0xff).unwrap_or(u8::MAX);
            config::Color::from_rgb(channel(16), channel(8), channel(0)).luma()
        };
        // Sampled inside the chrome card and inside the field, both well
        // clear of the centred modal: the two take different fills, and the
        // bug moved them in opposite directions.
        let stride = size.width.cast_unsigned() as usize;
        let card = painter.card().expect("the card is shown").bounds();
        let card_px = (card.bottom() - 8).cast_unsigned() as usize * stride
            + (card.right() - 8).cast_unsigned() as usize;
        let row = stride * 4;
        for (at, pane) in [(card_px, "the card"), (row + stride - 8, "the field")] {
            let (before, after) = (luma(resting[at]), luma(prompting[at]));
            assert!(
                after < before,
                "the scrim must darken {pane}: {before} -> {after}"
            );
            assert!(after > 0.0, "but must not black {pane} out");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Typing must be visible, or the user cannot tell the keyboard is
    /// reaching the field -- with the text concealed there is no other clue.
    #[test]
    fn typing_a_reply_changes_the_painted_prompt() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("password-typing");
        painter.browser.begin_password("Password: ", false);
        let empty = paint_opaque(&mut painter, size);

        for c in "hunter2".chars() {
            painter.browser.act(crate::keymap::Action::Insert(c));
        }
        let typed = paint_opaque(&mut painter, size);

        assert_ne!(empty, typed, "the bullets must appear as they are typed");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A warning from the helper must reach the screen: "2 attempts
    /// remaining" is the only notice before an account locks.
    #[test]
    fn a_notice_from_the_helper_changes_the_painted_prompt() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("password-notice");
        painter.browser.begin_password("Password: ", false);
        let quiet = paint_opaque(&mut painter, size);

        painter.browser.set_password_notice("2 attempts remaining");
        let warned = paint_opaque(&mut painter, size);

        assert_ne!(quiet, warned, "the warning must be visible");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Dismissing must restore the window exactly. A modal that leaves
    /// residue behind reads as a rendering fault.
    #[test]
    fn dismissing_the_prompt_restores_the_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("password-dismiss");
        let resting = paint_opaque(&mut painter, size);
        painter.browser.begin_password("Password: ", false);
        let _ = paint_opaque(&mut painter, size);
        painter.browser.end_password();
        let restored = paint_opaque(&mut painter, size);

        assert_eq!(resting, restored, "the modal must leave nothing behind");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The load-bearing rule: a click behind the modal must not reach the
    /// list. Without this a stray press selects a file -- or worse, a
    /// double-click opens one -- while the user believes they are typing a
    /// password.
    #[test]
    fn a_click_behind_the_prompt_does_not_reach_the_list() {
        let (mut painter, root, _size) = tiled("password-click");
        painter.browser.begin_password("Password: ", false);
        let at = tile_centre(&painter, "b_file.txt");
        left_press(&mut painter, at);
        assert!(
            painter.browser.selected_paths().is_empty(),
            "a click behind the modal must select nothing"
        );
        assert!(
            painter.take_pending_open().is_none(),
            "and must certainly not open anything"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn left_release(painter: &mut FilesWindow, at: super::Position) {
        painter.on_pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
    }

    /// A painter over the mixed directory with its tiles laid out.
    fn tiled(tag: &str) -> (FilesWindow, std::path::PathBuf, PhysicalSize) {
        let (mut painter, root, size) = painter_over_a_mixed_directory(tag);
        painter.sync_tiles();
        (painter, root, size)
    }

    /// The centre of the tile showing the entry named `name`.
    fn tile_centre(painter: &FilesWindow, name: &str) -> super::Position {
        let placed = painter
            .tile_layout()
            .into_iter()
            .find(|placed| {
                painter
                    .browser
                    .entry(placed.row)
                    .is_some_and(|entry| entry.name == name)
            })
            .expect("the entry has a tile");
        super::Position {
            x: placed.rect.x + placed.rect.w / 2,
            y: placed.rect.y + placed.rect.h / 2,
        }
    }

    #[test]
    fn every_visible_entry_gets_a_tile_inside_the_field() {
        let (painter, root, _size) = tiled("tiles-layout");
        let field = painter.frame().field();
        let placed = painter.tile_layout();
        assert_eq!(placed.len(), painter.browser.len());
        for tile in placed {
            assert!(tile.rect.x >= field.x && tile.rect.right() <= field.right());
            assert!(tile.rect.y >= field.y && tile.rect.bottom() <= field.bottom());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn one_click_on_a_folder_tile_only_selects_it() {
        let (mut painter, root, _size) = tiled("tiles-select-folder");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(
            painter.browser.path(),
            root,
            "a single click must not enter"
        );
        assert_eq!(painter.browser.selected_paths(), [root.join("a_directory")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_double_click_on_a_folder_tile_enters_it() {
        let (mut painter, root, _size) = tiled("tiles-enter");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        left_press(&mut painter, at);
        assert_eq!(painter.browser.path(), root.join("a_directory"));
        left_release(&mut painter, at);
        assert_eq!(painter.browser.path(), root.join("a_directory"));
        assert!(painter.browser.can_go_back(), "the step is in the history");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_slow_clicks_on_a_folder_tile_do_not_enter_it() {
        let (mut painter, root, _size) = tiled("tiles-slow");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        std::thread::sleep(std::time::Duration::from_millis(
            super::DOUBLE_CLICK_MS + 50,
        ));
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(painter.browser.path(), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_modified_click_extends_the_selection_and_never_opens() {
        let (mut painter, root, _size) = tiled("tiles-modified");
        let at = tile_centre(&painter, "a_directory");
        let ctrl = platform::toplevel::ModifierState::from_wire(4, 0, 0, 0);
        for _ in 0..2 {
            painter.on_pointer(PointerEventKind::Press {
                button: PointerButton::Left,
                at,
                modifiers: ctrl,
            });
            painter.on_pointer(PointerEventKind::Release {
                button: PointerButton::Left,
                at,
                modifiers: ctrl,
            });
        }
        assert_eq!(painter.browser.path(), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_second_click_of_a_folder_double_click_selects_nothing_inside() {
        let (mut painter, root, _size) = tiled("tiles-double");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(painter.browser.path(), root.join("a_directory"));
        assert!(painter.browser.selected_paths().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clicking_a_file_tile_selects_it_and_a_double_click_opens_it() {
        let (mut painter, root, _size) = tiled("tiles-file");
        let at = tile_centre(&painter, "b_file.txt");
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(painter.browser.selected_paths(), [root.join("b_file.txt")]);
        assert!(painter.take_pending_open().is_none());
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(painter.take_pending_open(), Some(root.join("b_file.txt")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dragging_a_folder_tile_carries_it_instead_of_entering() {
        let (mut painter, root, _size) = tiled("tiles-drag");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        painter.on_pointer(PointerEventKind::Motion(super::Position {
            x: at.x + 40,
            y: at.y + 40,
        }));
        assert_eq!(
            painter.take_pending_drag(),
            Some(vec![root.join("a_directory")])
        );
        left_release(&mut painter, at);
        assert_eq!(painter.browser.path(), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_drag_over_a_folder_tile_targets_it() {
        let (mut painter, root, _size) = tiled("tiles-drop");
        let folder = tile_centre(&painter, "a_directory");
        painter.update_drag_hover(true, (folder.x, folder.y));
        let row = painter.drop_hover_row.expect("a folder is a drop target");
        assert_eq!(painter.browser.entry(row).expect("row").name, "a_directory");
        painter.update_drag_hover(false, (folder.x, folder.y));
        assert_eq!(
            painter.drop_hover_row, None,
            "the highlight clears when the drag leaves"
        );
        let file = tile_centre(&painter, "b_file.txt");
        painter.update_drag_hover(true, (file.x, file.y));
        assert_eq!(painter.drop_hover_row, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hovering_a_tile_changes_the_frame() {
        let (mut painter, root, size) = tiled("tiles-hover");
        let resting = paint_opaque(&mut painter, size);
        let at = tile_centre(&painter, "b_file.txt");
        painter.on_pointer(PointerEventKind::Motion(at));
        assert!(painter.hover.spot().is_some());
        painter.step_motion(500);
        glide(&mut painter, 500, 20);
        assert_ne!(paint_opaque(&mut painter, size), resting);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A point on the tile named `name` clear of its buttons, near the top.
    fn tile_top(painter: &FilesWindow, name: &str) -> super::Position {
        let at = tile_centre(painter, name);
        let placed = painter.placed_at(at).expect("on a tile");
        super::Position {
            x: at.x,
            y: placed.rect.y + painter.theme.spacing.gutter() + 2,
        }
    }

    /// Point at the top of the tile named `name`, and let it settle.
    fn settle_on(painter: &mut FilesWindow, name: &str) -> super::Position {
        let at = tile_top(painter, name);
        painter.on_pointer(PointerEventKind::Motion(at));
        painter.step_motion(10_000);
        glide(painter, 10_000, 30);
        at
    }

    /// The middle of one of the settled tile's buttons, where it is drawn.
    fn button_centre(painter: &FilesWindow, name: &str, action: TileAction) -> super::Position {
        let placed = painter
            .tile_layout()
            .into_iter()
            .find(|placed| {
                painter
                    .browser
                    .entry(placed.row)
                    .is_some_and(|entry| entry.name == name)
            })
            .expect("the tile exists");
        let geometry = TileGeometry::of(
            placed.rect,
            painter.theme.spacing,
            painter.type_scale.tile_lines(),
        );
        let (_, rect) = geometry
            .bar
            .buttons()
            .iter()
            .find(|(a, _)| *a == action)
            .expect("this tile has room for its buttons");
        let lift = painter.theme.spacing.tile_lift();
        super::Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2 - lift,
        }
    }

    #[test]
    fn pointing_at_a_tile_lifts_it_and_reveals_its_buttons() {
        let (mut painter, root, _size) = tiled("tiles-reveal");
        settle_on(&mut painter, "b_file.txt");
        let row = (0..painter.browser.len())
            .find(|&r| {
                painter
                    .browser
                    .entry(r)
                    .is_some_and(|e| e.name == "b_file.txt")
            })
            .expect("the file is listed");
        assert_eq!(painter.motion.raise(TileKey::Item(row)), 255);
        assert_eq!(painter.motion.reveal(TileKey::Item(row)), 255);
        assert!(painter.motion.buttons_are_live(row));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_settled_tile_button_runs_its_action_on_that_tile_only() {
        let (mut painter, root, _size) = tiled("tiles-copy");
        settle_on(&mut painter, "b_file.txt");
        let at = button_centre(&painter, "b_file.txt", TileAction::Cut);
        painter.on_pointer(PointerEventKind::Motion(at));
        assert!(matches!(
            painter.hover.spot(),
            Some(Spot::Action(_, TileAction::Cut))
        ));
        left_press(&mut painter, at);
        assert!(
            !painter.browser.clipboard().is_cut(&root.join("b_file.txt")),
            "pressing alone does nothing yet"
        );
        left_release(&mut painter, at);
        assert!(
            painter.browser.clipboard().is_cut(&root.join("b_file.txt")),
            "releasing on the button cut its own tile"
        );
        assert!(
            painter.browser.selected_paths().is_empty(),
            "and the click did not also select it"
        );
        assert_eq!(
            painter.browser.notice(),
            None,
            "the footer is left alone; the button is the feedback"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sliding_off_a_held_button_before_releasing_cancels_it() {
        let (mut painter, root, _size) = tiled("tiles-cancel");
        settle_on(&mut painter, "b_file.txt");
        let on = button_centre(&painter, "b_file.txt", TileAction::Cut);
        painter.on_pointer(PointerEventKind::Motion(on));
        left_press(&mut painter, on);
        let away = button_centre(&painter, "b_file.txt", TileAction::CopyPath);
        painter.on_pointer(PointerEventKind::Motion(away));
        left_release(&mut painter, away);
        assert!(!painter.browser.clipboard().is_cut(&root.join("b_file.txt")));
        assert_eq!(
            painter.take_pending_copy(),
            None,
            "nor did it fire the other"
        );
        assert!(painter.pressed_action.is_none(), "the hold is over");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_held_button_takes_the_press_colour_and_shrinks_back_from_its_hover_size() {
        let (mut painter, root, size) = tiled("tiles-held");
        settle_on(&mut painter, "b_file.txt");
        let on = button_centre(&painter, "b_file.txt", TileAction::Copy);
        painter.on_pointer(PointerEventKind::Motion(on));
        painter.step_motion(50_000);
        glide(&mut painter, 50_000, 20);
        let hovered = paint_opaque(&mut painter, size);
        left_press(&mut painter, on);
        painter.step_motion(51_000);
        glide(&mut painter, 51_000, 20);
        let held = paint_opaque(&mut painter, size);
        assert_ne!(hovered, held, "holding the button changes how it looks");
        left_release(&mut painter, on);
        painter.step_motion(52_000);
        glide(&mut painter, 52_000, 20);
        assert_ne!(
            paint_opaque(&mut painter, size),
            held,
            "and releasing lets go"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hovered_button_is_larger_than_a_resting_one() {
        let (mut painter, root, size) = tiled("tiles-grow");
        settle_on(&mut painter, "b_file.txt");
        let beside = button_centre(&painter, "b_file.txt", TileAction::Copy);
        let resting = paint_opaque(&mut painter, size);
        let on = button_centre(&painter, "b_file.txt", TileAction::CopyPath);
        painter.on_pointer(PointerEventKind::Motion(on));
        painter.step_motion(60_000);
        glide(&mut painter, 60_000, 20);
        let grown = paint_opaque(&mut painter, size);
        let grow = painter.theme.spacing.action_grow();
        let edge = |buf: &[u32], x: i32, y: i32| buf[(y * size.width + x).cast_unsigned() as usize];
        let rect = {
            let placed = painter.placed_at(on).expect("on a tile");
            let geometry = TileGeometry::of(
                placed.rect,
                painter.theme.spacing,
                painter.type_scale.tile_lines(),
            );
            geometry.bar.buttons()[0].1
        };
        let lift = painter.theme.spacing.tile_lift();
        let probe_y = rect.y + rect.h / 2 - lift;
        let just_outside = rect.x - grow;
        assert_ne!(
            edge(&grown, just_outside, probe_y),
            edge(&resting, just_outside, probe_y),
            "the hovered button reaches {grow} px further than it rests"
        );
        assert_eq!(
            edge(&grown, beside.x, beside.y),
            edge(&resting, beside.x, beside.y),
            "the neighbouring button is unchanged"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_copy_path_button_queues_the_path_for_the_system_clipboard() {
        let (mut painter, root, _size) = tiled("tiles-copy-path");
        settle_on(&mut painter, "b_file.txt");
        let at = button_centre(&painter, "b_file.txt", TileAction::CopyPath);
        painter.on_pointer(PointerEventKind::Motion(at));
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(
            painter.take_pending_copy(),
            Some(root.join("b_file.txt").display().to_string())
        );
        assert_eq!(painter.take_pending_copy(), None, "taken once");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hovered_tile_offers_only_copy_path_copy_and_cut() {
        let (painter, root, _size) = tiled("tiles-three");
        let placed = painter
            .placed_at(tile_centre(&painter, "b_file.txt"))
            .expect("tile");
        let geometry = TileGeometry::of(
            placed.rect,
            painter.theme.spacing,
            painter.type_scale.tile_lines(),
        );
        let offered: Vec<TileAction> = geometry.bar.buttons().iter().map(|(a, _)| *a).collect();
        assert_eq!(offered, TileAction::PRIMARY);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn right_press(painter: &mut FilesWindow, at: super::Position) {
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Right,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
    }

    fn row_named(painter: &FilesWindow, name: &str) -> usize {
        (0..painter.browser.len())
            .find(|&r| painter.browser.entry(r).is_some_and(|e| e.name == name))
            .expect("the entry is listed")
    }

    /// Right-click the tile named `name` and let its tray open.
    fn open_tray_on(painter: &mut FilesWindow, name: &str) {
        let at = tile_top(painter, name);
        painter.on_pointer(PointerEventKind::Motion(at));
        right_press(painter, at);
        painter.step_motion(80_000);
        glide(painter, 80_000, 30);
    }

    /// The middle of a row button, where it is drawn.
    fn tray_centre(painter: &FilesWindow, action: TileAction) -> super::Position {
        let (row, tray) = painter.open_tray().expect("a tray is open");
        let (_, rect) = *tray
            .rows()
            .iter()
            .find(|(a, _)| *a == action)
            .expect("the tray holds the action");
        let lift = super::TilePainter::lift(
            painter.theme.spacing,
            painter.motion.raise(TileKey::Item(row)),
        );
        let offset = tray.offset(lift, painter.motion.tray(row));
        super::Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2 + offset,
        }
    }

    fn press_tray_button(painter: &mut FilesWindow, action: TileAction) {
        let at = tray_centre(painter, action);
        painter.on_pointer(PointerEventKind::Motion(at));
        left_press(painter, at);
        left_release(painter, at);
    }

    #[test]
    fn a_right_click_selects_the_tile_and_opens_a_tray_of_six() {
        let (mut painter, root, _size) = tiled("tray-open");
        assert!(painter.open_tray().is_none());
        open_tray_on(&mut painter, "b_file.txt");
        let (row, tray) = painter.open_tray().expect("the tray opened");
        assert_eq!(row, row_named(&painter, "b_file.txt"));
        let offered: Vec<TileAction> = tray.rows().iter().map(|(a, _)| *a).collect();
        assert_eq!(offered, TileAction::MENU);
        assert_eq!(painter.browser.selected_paths(), [root.join("b_file.txt")]);
        assert_eq!(painter.motion.tray(row), 255, "and it has finished opening");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_right_click_on_empty_space_opens_no_tray() {
        let (mut painter, root, _size) = tiled("tray-empty");
        let field = painter.frame().field();
        let blank = super::Position {
            x: field.x + 1,
            y: field.bottom() - 1,
        };
        if painter.entry_at((blank.x, blank.y)).is_none() {
            right_press(&mut painter, blank);
            assert!(painter.open_tray().is_none());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_closes_on_a_click_elsewhere_and_on_escape() {
        let (mut painter, root, _size) = tiled("tray-close");
        open_tray_on(&mut painter, "b_file.txt");
        let away = tile_top(&painter, "a_directory");
        left_press(&mut painter, away);
        left_release(&mut painter, away);
        assert!(painter.open_tray().is_none(), "a click outside closes it");
        open_tray_on(&mut painter, "b_file.txt");
        painter.on_key(KeyPress {
            key: platform::toplevel::Key::Escape,
            modifiers: platform::toplevel::ModifierState::default(),
            repeat: false,
        });
        assert!(painter.open_tray().is_none(), "Escape closes it");
        assert_eq!(
            painter.browser.selected_paths(),
            [root.join("b_file.txt")],
            "and that Escape did not also clear the selection"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_click_on_the_open_card_but_off_its_buttons_keeps_it_open() {
        let (mut painter, root, _size) = tiled("tray-inside");
        open_tray_on(&mut painter, "b_file.txt");
        let (row, tray) = painter.open_tray().expect("open");
        let card = painter.tray_card(row, &tray);
        let corner = super::Position {
            x: card.x + 1,
            y: card.bottom() - 1,
        };
        left_press(&mut painter, corner);
        left_release(&mut painter, corner);
        assert!(painter.open_tray().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_follows_a_relisting_by_closing() {
        let (mut painter, root, _size) = tiled("tray-relist");
        open_tray_on(&mut painter, "b_file.txt");
        std::fs::write(root.join("new.txt"), b"x").expect("write");
        let _ = painter.browser.act(crate::keymap::Action::Refresh);
        painter.sync_tiles();
        assert!(painter.open_tray().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_rename_opens_the_rename_box_and_closes_the_tray() {
        let (mut painter, root, _size) = tiled("tray-rename");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::Rename);
        assert!(painter.browser.is_renaming());
        assert!(painter.open_tray().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_trash_moves_only_that_entry_to_the_trash() {
        let (mut painter, root, _size) = tiled("tray-trash");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        painter
            .browser
            .set_trash(fileops::Trash::at(fileops::TrashDir::at(bin)));
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::Trash);
        assert!(!root.join("b_file.txt").exists(), "the entry is gone");
        assert!(root.join("a_directory").exists(), "its neighbour is not");
        assert!(painter.open_tray().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_duplicate_copies_the_entry_beside_itself() {
        let (mut painter, root, _size) = tiled("tray-duplicate");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::Duplicate);
        assert!(root.join("b_file.txt").exists(), "the original stays");
        let copies = std::fs::read_dir(&root)
            .expect("read")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("b_file"))
            .count();
        assert_eq!(copies, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_copy_name_queues_the_bare_name() {
        let (mut painter, root, _size) = tiled("tray-name");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::CopyName);
        assert_eq!(painter.take_pending_copy().as_deref(), Some("b_file.txt"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_open_enters_a_folder_and_opens_a_file() {
        let (mut painter, root, _size) = tiled("tray-open-action");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::Open);
        assert_eq!(painter.take_pending_open(), Some(root.join("b_file.txt")));
        open_tray_on(&mut painter, "a_directory");
        press_tray_button(&mut painter, TileAction::Open);
        assert_eq!(painter.browser.path(), root.join("a_directory"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_tray_move_to_takes_a_typed_folder_and_moves_the_entry_there() {
        let (mut painter, root, _size) = tiled("tray-move");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::MoveTo);
        assert!(painter.browser.is_moving(), "the box has the keyboard");
        assert!(painter.open_tray().is_none());
        let typed = "a_directory";
        for c in typed.chars() {
            painter.on_key(KeyPress {
                key: platform::toplevel::Key::Char(c),
                modifiers: platform::toplevel::ModifierState::default(),
                repeat: false,
            });
        }
        assert!(painter.browser.move_target_is_valid());
        painter.on_key(KeyPress {
            key: platform::toplevel::Key::Enter,
            modifiers: platform::toplevel::ModifierState::default(),
            repeat: false,
        });
        assert!(!painter.browser.is_moving());
        assert!(!root.join("b_file.txt").exists());
        assert!(root.join("a_directory").join("b_file.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_move_to_a_missing_folder_stays_open_and_moves_nothing() {
        let (mut painter, root, _size) = tiled("tray-move-missing");
        open_tray_on(&mut painter, "b_file.txt");
        press_tray_button(&mut painter, TileAction::MoveTo);
        for c in "nowhere".chars() {
            painter.on_key(KeyPress {
                key: platform::toplevel::Key::Char(c),
                modifiers: platform::toplevel::ModifierState::default(),
                repeat: false,
            });
        }
        assert!(!painter.browser.move_target_is_valid());
        painter.on_key(KeyPress {
            key: platform::toplevel::Key::Enter,
            modifiers: platform::toplevel::ModifierState::default(),
            repeat: false,
        });
        assert!(painter.browser.is_moving(), "still open to be corrected");
        assert!(root.join("b_file.txt").exists());
        painter.on_key(KeyPress {
            key: platform::toplevel::Key::Escape,
            modifiers: platform::toplevel::ModifierState::default(),
            repeat: false,
        });
        assert!(!painter.browser.is_moving());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_rows_are_drawn_on_the_tiles_own_card_under_its_buttons() {
        let (mut painter, root, size) = tiled("tray-paint");
        let placed = painter
            .placed_at(tile_top(&painter, "b_file.txt"))
            .expect("tile");
        let face = painter.theme.spacing.tile_face(placed.rect);
        let before = paint_opaque(&mut painter, size);
        open_tray_on(&mut painter, "b_file.txt");
        let (row, tray) = painter.open_tray().expect("open");
        let card = painter.tray_card(row, &tray);
        assert_eq!((card.x, card.w), (face.x, face.w), "the same card");
        assert!(card.h >= face.h);
        let geometry = TileGeometry::of(
            placed.rect,
            painter.theme.spacing,
            painter.type_scale.tile_lines(),
        );
        let buttons_bottom = geometry.bar.buttons()[0].1.bottom();
        for (_, rect) in tray.rows() {
            assert!(rect.y > buttons_bottom, "under the hover buttons");
        }
        let lift = super::TilePainter::lift(
            painter.theme.spacing,
            painter.motion.raise(TileKey::Item(row)),
        );
        let last = tray.rows()[5].1;
        let low = last.bottom() + tray.offset(lift, 255) - 2;
        let at = low * size.width + last.x + last.w / 2;
        let after = paint_opaque(&mut painter, size);
        assert_ne!(
            before[at.cast_unsigned() as usize],
            after[at.cast_unsigned() as usize],
            "the last row is drawn"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_press_before_the_buttons_have_appeared_selects_the_tile_instead() {
        let (mut painter, root, _size) = tiled("tiles-early");
        let at = tile_centre(&painter, "b_file.txt");
        painter.on_pointer(PointerEventKind::Motion(at));
        painter.step_motion(20_000);
        let button = button_centre(&painter, "b_file.txt", TileAction::Cut);
        painter.on_pointer(PointerEventKind::Motion(button));
        left_press(&mut painter, button);
        left_release(&mut painter, button);
        assert!(
            !painter.browser.clipboard().is_cut(&root.join("b_file.txt")),
            "an invisible button must not fire"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tile_with_no_room_for_buttons_is_clicked_as_before() {
        let (mut painter, root) = crowded("tiles-narrow", 60);
        let small = painter.tile_layout().into_iter().find(|placed| {
            TileGeometry::of(
                placed.rect,
                painter.theme.spacing,
                painter.type_scale.tile_lines(),
            )
            .bar
            .buttons()
            .is_empty()
        });
        if let Some(placed) = small {
            let at = super::Position {
                x: placed.rect.x + placed.rect.w / 2,
                y: placed.rect.y + placed.rect.h / 2,
            };
            painter.on_pointer(PointerEventKind::Motion(at));
            assert!(matches!(
                painter.hover.spot(),
                Some(Spot::Item(Pane::Tiles, _))
            ));
            painter.step_motion(30_000);
            glide(&mut painter, 30_000, 30);
            assert!(painter.live_action_at(at).is_none());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn leaving_the_window_lets_the_tile_settle_back() {
        let (mut painter, root, size) = tiled("tiles-leave");
        let resting = paint_opaque(&mut painter, size);
        let at = settle_on(&mut painter, "b_file.txt");
        assert_ne!(paint_opaque(&mut painter, size), resting);
        painter.on_pointer(PointerEventKind::Leave);
        painter.step_motion(40_000);
        glide(&mut painter, 40_000, 30);
        assert_eq!(paint_opaque(&mut painter, size), resting, "from {at:?}");
        assert!(!painter.motion.is_animating());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn crowded(tag: &str, files: usize) -> (FilesWindow, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");
        for i in 0..files {
            std::fs::write(root.join(format!("f{i:03}")), vec![0; 10 + i]).expect("file");
        }
        let cfg = config::Config::default();
        let favorites = crate::favorites::Favorites::default();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs::UserDirs::load(), &favorites);
        let mut painter = FilesWindow::new(
            crate::browser::Browser::open(&root, 700, 400, 28).expect("listable"),
            tiles::theme::Theme::from_config(&cfg),
            tiles::typography::Typography::load(&cfg, 1.0),
            sidebar,
            favorites,
            PhysicalSize {
                width: 900,
                height: 600,
            },
            waker::Waker::new().expect("an eventfd"),
        );
        painter.sync_tiles();
        (painter, root)
    }

    fn in_view(painter: &FilesWindow) -> Vec<String> {
        painter
            .tile_layout()
            .iter()
            .filter_map(|placed| painter.browser.entry(placed.row))
            .map(|entry| entry.name.clone())
            .collect()
    }

    fn key(painter: &mut FilesWindow, key: platform::toplevel::Key, shift: bool) {
        let modifiers = if shift {
            platform::toplevel::ModifierState::from_wire(1, 0, 0, 0)
        } else {
            platform::toplevel::ModifierState::default()
        };
        painter.on_key(platform::toplevel::KeyPress {
            key,
            modifiers,
            repeat: false,
        });
    }

    fn cursor_name(painter: &FilesWindow) -> String {
        painter
            .browser
            .cursor_entry()
            .expect("a cursor")
            .name
            .clone()
    }

    #[test]
    fn arrow_keys_walk_the_tiles_largest_first() {
        let (mut painter, root) = crowded("tiles-keys", 3);
        key(&mut painter, platform::toplevel::Key::Down, false);
        assert_eq!(
            cursor_name(&painter),
            "f002",
            "the largest file comes first"
        );
        key(&mut painter, platform::toplevel::Key::Down, false);
        assert_eq!(cursor_name(&painter), "f001");
        key(&mut painter, platform::toplevel::Key::Up, false);
        assert_eq!(cursor_name(&painter), "f002");
        key(&mut painter, platform::toplevel::Key::End, false);
        assert_eq!(cursor_name(&painter), "f000");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shift_arrows_extend_the_selection_along_the_spiral() {
        let (mut painter, root) = crowded("tiles-keys-shift", 5);
        key(&mut painter, platform::toplevel::Key::Down, false);
        key(&mut painter, platform::toplevel::Key::Down, true);
        key(&mut painter, platform::toplevel::Key::Down, true);
        let mut names: Vec<String> = painter
            .browser
            .selected_paths()
            .iter()
            .map(|p| p.file_name().expect("name").to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["f002", "f003", "f004"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn walking_off_the_last_tile_scrolls_one_tile_on() {
        let (mut painter, root) = crowded("tiles-keys-scroll", 60);
        let before = in_view(&painter);
        assert!(before.len() < 60, "sixty files overflow a 900x600 window");
        key(&mut painter, platform::toplevel::Key::End, false);
        let last = cursor_name(&painter);
        assert_eq!(Some(&last), before.last());
        key(&mut painter, platform::toplevel::Key::Down, false);
        let after = in_view(&painter);
        assert_eq!(&after[..after.len() - 1], &before[1..]);
        assert_eq!(
            Some(cursor_name(&painter)).as_ref(),
            after.last(),
            "the cursor lands on the tile that came in"
        );
        key(&mut painter, platform::toplevel::Key::Up, false);
        assert_eq!(
            cursor_name(&painter),
            last,
            "walking back lands where it left"
        );
        key(&mut painter, platform::toplevel::Key::Up, false);
        key(&mut painter, platform::toplevel::Key::Home, false);
        key(&mut painter, platform::toplevel::Key::Up, false);
        assert_eq!(in_view(&painter)[0], before[0], "the top is where it began");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn wheel(painter: &mut FilesWindow, lines: i32) {
        let field = painter.frame().field();
        painter.on_pointer(PointerEventKind::Scroll {
            lines,
            at: super::Position {
                x: field.x + field.w / 2,
                y: field.y + field.h / 2,
            },
            modifiers: platform::toplevel::ModifierState::default(),
        });
    }

    #[test]
    fn each_wheel_click_drops_one_tile_from_the_large_end_and_adds_one_small() {
        let (mut painter, root) = crowded("tiles-wheel", 60);
        let before = in_view(&painter);
        wheel(&mut painter, -1);
        assert_eq!(in_view(&painter), before, "nothing lies above the top");
        wheel(&mut painter, 1);
        let one = in_view(&painter);
        assert_eq!(&one[..one.len() - 1], &before[1..]);
        wheel(&mut painter, 2);
        let three = in_view(&painter);
        assert_eq!(&three[..three.len() - 3], &before[3..before.len()]);
        wheel(&mut painter, -3);
        assert_eq!(
            in_view(&painter),
            before,
            "scrolling back restores the view"
        );
        assert_eq!(
            painter.browser.path(),
            root,
            "the wheel never leaves the folder"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn centre_of(rect: platform::draw::Rect) -> super::Position {
        super::Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2,
        }
    }

    #[test]
    fn a_crowded_folder_shows_a_more_tile_counting_what_is_out_of_view() {
        let (painter, root) = crowded("tiles-more", 60);
        let more = painter.tile_more().expect("sixty files do not fit");
        assert_eq!(more.hidden, 60 - in_view(&painter).len());
        let tiles = painter.painted_tiles(&painter.tile_layout());
        let drawn = tiles.last().expect("tiles");
        assert_eq!(drawn.name, format!("{} more…", more.hidden));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_fits_has_no_more_tile() {
        let (painter, root) = crowded("tiles-no-more", 3);
        assert!(painter.tile_more().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pressing_the_more_tile_shows_the_next_tiles() {
        let (mut painter, root) = crowded("tiles-more-press", 60);
        let before = in_view(&painter);
        let at = centre_of(painter.tile_more().expect("more").rect);
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        let after = in_view(&painter);
        assert_ne!(after, before);
        assert!(!before.contains(&after[0]), "a whole page on, not one tile");
        assert_eq!(painter.browser.path(), root, "the folder is unchanged");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_more_tile_is_gone_once_the_smallest_tile_is_in_view() {
        let (mut painter, root) = crowded("tiles-more-end", 60);
        wheel(&mut painter, 500);
        assert!(painter.tile_more().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hovering_the_more_tile_lights_it_and_nothing_else() {
        let (painter, root) = crowded("tiles-more-hover", 60);
        let at = centre_of(painter.tile_more().expect("more").rect);
        assert_eq!(painter.spot_at(at), Some(Spot::More));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_more_tile_has_no_action_buttons() {
        let (mut painter, root) = crowded("tiles-more-buttons", 60);
        let at = centre_of(painter.tile_more().expect("more").rect);
        assert_eq!(painter.live_action_at(at), None);
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Right,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
        assert!(painter.tray.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_wheel_stops_when_the_smallest_tile_is_in_view() {
        let (mut painter, root) = crowded("tiles-wheel-end", 60);
        wheel(&mut painter, 500);
        let end = in_view(&painter);
        assert_eq!(end.last().map(String::as_str), Some("f000"));
        wheel(&mut painter, 1);
        assert_eq!(in_view(&painter), end);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scrolling_makes_the_new_tile_grow_in_and_the_old_one_shrink_out() {
        let (mut painter, root) = crowded("tiles-wheel-glide", 60);
        let size = PhysicalSize {
            width: 900,
            height: 600,
        };
        let arrived = glide(&mut painter, 0, 150);
        assert!(!painter.growth.is_animating());
        let largest = in_view(&painter)[0].clone();
        wheel(&mut painter, 1);
        painter.step_motion(arrived + 16);
        assert!(
            painter.growth.is_animating(),
            "the scroll is followed by a glide"
        );
        let shrinking: Vec<String> = painter
            .growth
            .leaving()
            .iter()
            .filter_map(|(_, row, _)| painter.browser.entry(*row))
            .map(|entry| entry.name.clone())
            .collect();
        assert_eq!(shrinking, [largest]);
        let mid = paint_opaque(&mut painter, size);
        glide(&mut painter, arrived + 16, 150);
        assert!(painter.growth.leaving().is_empty());
        assert_ne!(mid, paint_opaque(&mut painter, size));
        let _ = std::fs::remove_dir_all(&root);
    }
}
