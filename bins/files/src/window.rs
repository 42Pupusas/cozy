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
use platform::toplevel::{KeyPress, Painter, PhysicalSize, PointerEventKind, Position};
use std::os::fd::RawFd;
use widget::{DragArm, Mods};

use crate::breadcrumb::Breadcrumb;
use crate::browser::{Browser, Outcome};
use crate::drag_badge::{BadgeStyle, DragBadge};
use crate::favorites::Favorites;
use crate::frame::Frame;
use crate::hover::{Hover, Pane, Spot};
use crate::pinned_card::PinnedCard;
use crate::row_format::RowFormat;
use crate::row_kind::RowKind;
use crate::sidebar::{Sidebar, SidebarRow};
use crate::spiral::MinCell;
use crate::spiral_cursor::{Landing, SpiralCursor};
use volumes::mount_backend::MountOutcome;
use volumes::{AttachedDevice, VolumeFlush, VolumeWatch};

use crate::mount_service::MountProgress;
use crate::mount_session::{MountIdentity, MountSession};
use crate::password_panel::{PanelStyle, PanelText, PasswordPanel};
use crate::storage::Storage;
use crate::theme::Theme;
use crate::tile_field::{PlacedTile, TileField};
use crate::tile_painter::{PaintedTile, TileFace, TilePainter, TileRename};
use crate::tile_rank::Tile;
use crate::toolbar::{Target, Toolbar};
use crate::typography::Typography;

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

/// Where one sidebar entry sits, and in what state.
///
/// A struct rather than positional arguments: `x`, `y`, `width` and `row_h`
/// are all `i32`, and `active` and `hovered` are both `bool`, so a transposed
/// pair would compile and paint the wrong thing.
#[derive(Clone, Copy)]
struct SidebarEntry {
    x: i32,
    y: i32,
    width: i32,
    row_h: i32,
    active: bool,
    hovered: bool,
}

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
    mount_candidate: Option<AttachedDevice>,
    /// Watches the mount table so attached devices appear and disappear.
    ///
    /// Held here rather than in the event loop because the sidebar it feeds
    /// lives here, and a poll that found a change would otherwise have to
    /// hand the new set across the same seam the loop already crosses for
    /// drags and opens.
    volumes: VolumeWatch,
    /// The tile presentation: size ranking, overflow pages, armed folder.
    tiles: TileField,
    /// How many places rows the sidebar's list is scrolled past. The compact
    /// card shows a handful of rows; the wheel over the card moves this.
    places_scroll: i32,
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
            places_scroll: 0,
        }
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
        self.tile_at(Point::new(at.0, at.1)).and_then(Tile::item)
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
                .card_cell(frame.field(), self.theme.spacing.tile_min(), Some(need))?;
        Some(frame.card_in(cell))
    }

    fn tile_layout(&self) -> Vec<PlacedTile> {
        let (spiral, tail) = self.flow_parts();
        self.tiles
            .layout(spiral, self.theme.spacing.tile_min(), tail)
    }

    fn tile_at(&self, at: Point) -> Option<Tile> {
        let (spiral, tail) = self.flow_parts();
        self.tiles
            .hit(spiral, self.theme.spacing.tile_min(), tail, at)
    }

    /// Bring the tile ranking up to date with the browser.
    fn sync_tiles(&mut self) {
        self.tiles.sync(&self.browser);
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

    /// Fold in folder totals the size worker produced. Returns whether the
    /// tiles must be redrawn.
    pub fn poll_tiles(&mut self) -> bool {
        let arrived = self.tiles.poll();
        self.tiles.sync(&self.browser);
        arrived
    }

    /// Whether the up control has somewhere to go: out of an overflow page,
    /// or to the parent directory.
    fn can_step_out(&self) -> bool {
        self.tiles.depth() > 0 || self.browser.can_go_up()
    }

    /// Close the innermost overflow page if one is open, else go up a
    /// directory.
    fn step_out(&mut self) -> Outcome {
        if self.tiles.close_page() {
            return Outcome::Redraw;
        }
        self.browser.act(crate::keymap::Action::GoUp)
    }

    /// A movement key in the tile view: walk the spiral in rank order,
    /// crossing into and out of overflow pages at their edges.
    fn steer_tiles(&mut self, command: widget::ListCommand) -> Outcome {
        let (spiral, tail) = self.flow_parts();
        let (page, more) = self.tiles.page(spiral, self.theme.spacing.tile_min(), tail);
        let cursor = self.browser.pane().cursor();
        let landing =
            SpiralCursor::new(&page, more.is_some(), self.tiles.depth() > 0).land(cursor, command);
        let extend = matches!(command, widget::ListCommand::Extend(_));
        match landing {
            Landing::At(row) => self.browser.steer_to(row, extend),
            Landing::NextPage => {
                let Some(from) = more else {
                    return Outcome::Ignored;
                };
                self.tiles.open_more(from);
                self.steer_to_page_edge(true, extend)
            }
            Landing::PrevPage => {
                self.tiles.close_page();
                self.steer_to_page_edge(false, extend)
            }
            Landing::Stay => Outcome::Ignored,
        }
    }

    /// After a page change, put the cursor on the new page's first entry
    /// when walking forward, or its last when walking back.
    fn steer_to_page_edge(&mut self, first: bool, extend: bool) -> Outcome {
        let (spiral, tail) = self.flow_parts();
        let (page, _) = self.tiles.page(spiral, self.theme.spacing.tile_min(), tail);
        let edge = if first { page.first() } else { page.last() };
        match edge {
            Some(&row) => {
                let _ = self.browser.steer_to(row, extend);
                Outcome::Redraw
            }
            None => Outcome::Redraw,
        }
    }

    /// A left press anywhere in the window.
    ///
    /// The card's controls are checked before anything else, and a click in
    /// the places list goes no further, so neither can fall through to an
    /// entry lying under it.
    fn press_left(&mut self, at: Position, mods: Mods) {
        if self.tiles.swallows(Self::now_ms(), DOUBLE_CLICK_MS) {
            self.last_click = None;
            return;
        }
        if let Some(target) = self.toolbar_hit(at) {
            self.activate_control(target);
            return;
        }
        if self.in_sidebar(at) {
            self.press_sidebar(at);
            return;
        }
        let over_card = self
            .card()
            .is_some_and(|card| card.bounds().contains(Point::new(at.x, at.y)));
        if !over_card && self.frame().field().contains(Point::new(at.x, at.y)) {
            self.press_tile(at, mods);
        }
    }

    /// A left press on the places list: attach an unmounted device, or go to
    /// the place.
    ///
    /// The device check comes first and consumes the click: a device row has
    /// no path, so falling through would find nothing to navigate to and the
    /// row would appear dead.
    fn press_sidebar(&mut self, at: Position) {
        let Some(index) = self.sidebar_row_at(at) else {
            return;
        };
        if self.mount_from_sidebar(index) {
            return;
        }
        if let Some(path) = self
            .sidebar
            .rows()
            .get(index)
            .and_then(SidebarRow::path)
            .map(std::path::Path::to_path_buf)
        {
            let outcome = self.browser.go_to(&path);
            self.apply(outcome);
            // A new directory may live on a different filesystem.
            self.refresh_storage();
        }
    }

    /// A left press in the tile field.
    ///
    /// A folder enters on release rather than on press, so pressing and
    /// dragging it carries it instead of opening it. Files need a
    /// double-click to open, as in the other presentations.
    fn press_tile(&mut self, at: Position, mods: Mods) {
        let now = Self::now_ms();
        let doubled = self.is_double_click(now, at);
        match self.tile_at(Point::new(at.x, at.y)) {
            None => {
                let outcome = self.browser.clear_selection();
                self.apply(outcome);
            }
            Some(Tile::More { from, .. }) => {
                self.tiles.open_more(from);
                let _ = self.hover.clear();
            }
            Some(Tile::Item(row)) => {
                let navigable = self.browser.navigable_row(Some(row)).is_some();
                let outcome = if doubled && !navigable {
                    self.browser.activate_entry(row)
                } else {
                    self.browser.pick(row, mods)
                };
                self.apply(outcome);
                if navigable && mods == Mods::none() {
                    self.tiles.arm(row);
                }
                self.drag_row = Some(row);
                self.drag_arm.press((at.x, at.y));
            }
        }
    }

    /// Enter the folder a tile press armed, if the gesture stayed a click.
    fn release_tile(&mut self) {
        let Some(row) = self.tiles.take_armed() else {
            return;
        };
        let outcome = self.browser.activate_entry(row);
        self.apply(outcome);
        self.tiles.entered(Self::now_ms());
    }

    /// The tiles of the open page, gathered for painting.
    fn painted_tiles(&self, placed: &[PlacedTile]) -> Vec<PaintedTile> {
        placed
            .iter()
            .filter_map(|placed| match placed.tile {
                Tile::Item(row) => self.painted_entry_tile(placed.rect, row),
                Tile::More { from, count } => {
                    Some(self.painted_overflow_tile(placed.rect, from, count))
                }
            })
            .collect()
    }

    fn painted_entry_tile(&self, rect: Rect, row: usize) -> Option<PaintedTile> {
        let entry = self.browser.entry(row)?;
        let sizes = self.tiles.sizes();
        let detail = if sizes.is_pending(entry) {
            String::from("measuring…")
        } else {
            RowFormat::human_size(sizes.of(entry))
        };
        let pane = self.browser.pane();
        let cut = self.browser.clipboard().is_cut(&entry.path);
        Some(PaintedTile {
            rect,
            face: TileFace::Item(RowKind::of(entry)),
            name: entry.name.clone(),
            detail,
            name_color: if cut {
                self.theme.muted
            } else {
                RowFormat::color(entry, &self.theme)
            },
            selected: pane.is_selected(row),
            hovered: self.hover.is(Pane::Tiles, row)
                || (self.focused && pane.cursor() == Some(row)),
            drop_target: self.drop_hover_row == Some(row),
        })
    }

    fn painted_overflow_tile(&self, rect: Rect, from: usize, count: usize) -> PaintedTile {
        let sizes = self.tiles.sizes();
        let bytes: u64 = self
            .tiles
            .ranked()
            .get(from..)
            .unwrap_or_default()
            .iter()
            .filter_map(|&row| self.browser.entry(row))
            .map(|entry| sizes.of(entry))
            .sum();
        PaintedTile {
            rect,
            face: TileFace::Overflow,
            name: format!("{count} more…"),
            detail: RowFormat::human_size(bytes),
            name_color: self.theme.text,
            selected: false,
            hovered: self.hover.is_overflow(),
            drop_target: false,
        }
    }

    fn tile_rename(&self, placed: &[PlacedTile]) -> Option<TileRename> {
        let editing = self.painted_rename()?;
        let tile = placed
            .iter()
            .find(|placed| placed.tile == Tile::Item(editing.row))?;
        Some(TileRename {
            tile: tile.rect,
            text: editing.text,
            before_caret: editing.before_caret,
            valid: editing.valid,
        })
    }

    fn paint_tiles(&mut self, canvas: &mut Canvas<'_>) {
        let placed = self.tile_layout();
        let tiles = self.painted_tiles(&placed);
        let rename = self.tile_rename(&placed);
        let mut brush = TilePainter::new(&self.theme, &mut self.type_scale);
        brush.paint(canvas, &tiles);
        if let Some(rename) = rename {
            brush.paint_rename(canvas, &rename);
        }
    }

    /// Re-read the filesystem figures behind the sidebar footer.
    ///
    /// Called on navigation rather than per paint: crossing into a different
    /// mount is the only thing that can change which filesystem is being
    /// reported, and the fill level moves far slower than the frame rate.
    fn refresh_storage(&mut self) {
        let path = self.browser.path().to_path_buf();
        // A device the user has navigated into is named by its label rather
        // than by its mount point: "KINGSTON" is what is written on the
        // stick, while `/run/media/user/KINGSTON` is where this machine
        // happened to attach it.
        let named = self
            .volumes
            .volumes()
            .containing(&path)
            .map(|volume| volume.label().to_owned());
        self.storage = Storage::of(&path).map(|storage| match named {
            Some(label) => storage.named(label),
            None => storage,
        });
    }

    /// Poll for attached or removed devices, rebuilding the sidebar on a
    /// change. Returns whether anything changed, so the loop can repaint.
    ///
    /// Driven from the event loop rather than from `paint`: painting is the
    /// consequence of a change, and a poll inside it would read the mount
    /// table on every frame of a scroll.
    pub fn poll_volumes(&mut self) -> bool {
        if !self.volumes.poll_if_due().needs_redraw() {
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

    /// Scroll the places list by wheel `lines`, clamped to its content.
    /// No-op without a card; the pointer event itself dirties the frame.
    fn scroll_places(&mut self, lines: i32) {
        let Some(card) = self.card() else {
            return;
        };
        let row_h = self.theme.sidebar_row();
        if row_h <= 0 {
            return;
        }
        let rows = i32::try_from(self.sidebar.rows().len()).unwrap_or(0);
        let visible_rows = (card.places().h / row_h).max(1);
        let max = (rows - visible_rows).max(0);
        self.places_scroll = (self.places_scroll + lines).clamp(0, max);
    }

    /// Whether `at` falls inside the card's places list. The storage footer
    /// is a readout, not a control, so it is excluded.
    fn in_sidebar(&self, at: Position) -> bool {
        self.card().is_some_and(|card| {
            card.places()
                .contains(platform::draw::Point::new(at.x, at.y))
        })
    }

    /// Which card control is under `at`, if any.
    fn toolbar_hit(&self, at: Position) -> Option<Target> {
        self.toolbar().and_then(|bar| bar.hit(at.x, at.y))
    }

    /// What the pointer at `at` is over: a control, or a row in one of the panes.
    ///
    /// The regions are tested in the same order the press handler tests them,
    /// and only one can win, so the highlight cannot appear in two places at
    /// once -- and the thing that lights up is the thing a click would hit.
    fn spot_at(&self, at: Position) -> Option<Spot> {
        if let Some(target) = self.toolbar_hit(at) {
            return Some(Spot::Control(target));
        }
        if self.in_sidebar(at) {
            return self
                .sidebar_row_at(at)
                .map(|i| Spot::Item(Pane::Sidebar, i));
        }
        self.tile_at(Point::new(at.x, at.y)).map(|tile| {
            tile.item()
                .map_or(Spot::Overflow, |i| Spot::Item(Pane::Tiles, i))
        })
    }

    /// Act on a click on toolbar control `target`.
    fn activate_control(&mut self, target: Target) {
        let outcome = match target {
            Target::Back => self.browser.go_back(),
            Target::Forward => self.browser.go_forward(),
            // The same action the keyboard's Left and Backspace already
            // reach, so the button and the key cannot diverge.
            Target::Up => self.step_out(),
            Target::ToggleHidden => self.browser.act(crate::keymap::Action::ToggleHidden),
            Target::FilterBox => self.browser.focus_filter(),
        };
        self.apply(outcome);
        // Every navigation can cross a filesystem boundary, exactly as a
        // sidebar jump can, so the storage footer must be re-read.
        if matches!(target, Target::Back | Target::Forward | Target::Up) {
            self.refresh_storage();
        }
    }

    /// Which sidebar row a pointer position falls on. Shared by the press
    /// handler, the hover tracker and the painter, so all three agree.
    fn sidebar_row_at(&self, at: Position) -> Option<usize> {
        let card = self.card()?;
        let row_h = self.theme.sidebar_row();
        let y = at.y - card.places().y + self.places_scroll * row_h;
        self.sidebar.row_index_at(y, row_h)
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
        let theme = self.theme;
        let radius = theme.spacing.radius() * 2;
        canvas.fill_round_rect_glass(card.bounds(), theme.sidebar_bg, radius);
        canvas.draw_round_rect(card.bounds(), theme.sidebar_border, radius);
        self.paint_nav_buttons(canvas);
        self.paint_hidden_toggle(canvas);
        self.paint_breadcrumb(canvas);
        self.paint_filter_control(canvas);
        self.paint_sidebar(canvas);
        self.paint_storage(canvas);
        self.paint_status(canvas);
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
        ] {
            // Dropped for want of room: nothing to draw, and `Toolbar::hit`
            // will not report it either.
            let Some(rect) = rect else { continue };
            // Only a button that would do something lights up under the
            // pointer, so the highlight is a promise rather than decoration.
            let hovered = enabled && self.hover.control() == Some(target);
            let bg = if hovered {
                canvas.fill_round_rect(rect, theme.hover, theme.spacing.radius_small());
                theme.hover
            } else {
                theme.sidebar_bg
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
        let bg = if on {
            canvas.fill_round_rect(rect, theme.accent_surface, radius);
            theme.accent_surface
        } else if hovered {
            canvas.fill_round_rect(rect, theme.hover, radius);
            theme.hover
        } else {
            theme.sidebar_bg
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

        let (shown, elided) = crumbs.tail(self.crumb_budget(bar.w));
        if elided {
            let label = &mut self.type_scale.label;
            let y = label.baseline_y(bar.h) + bar.y;
            x = label.draw_text(
                canvas,
                Breadcrumb::ELLIPSIS,
                x,
                y,
                theme.muted,
                theme.sidebar_bg,
            );
            x += theme.spacing.xs();
        }

        for (i, crumb) in shown.iter().enumerate() {
            // A separator between segments, but never before the first: the
            // root crumb already reads as "/".
            if i > 0 || elided {
                let label = &mut self.type_scale.label;
                let y = label.baseline_y(bar.h) + bar.y;
                x = label.draw_text(canvas, "/", x, y, theme.muted, theme.sidebar_bg);
                x += theme.spacing.xs();
            }
            let (color, tier) = if crumb.current {
                (theme.text, &mut self.type_scale.title)
            } else {
                (theme.muted, &mut self.type_scale.body)
            };
            let y = tier.baseline_y(bar.h) + bar.y;
            x = tier.draw_text(canvas, &crumb.label, x, y, color, theme.sidebar_bg);
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
        canvas.fill_round_rect(rect, theme.filter_bg, radius);
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
            detail.draw_text(canvas, &query, inner_x, text_y, theme.text, theme.filter_bg);
            // Measured, not counted: with a proportional face the caret's
            // pixel offset is the width of the text before it.
            let caret_x = inner_x + detail.measure(&before);
            canvas.fill_rect(
                Rect::new(caret_x, text_y, theme.spacing.rule(), detail.line_height()),
                theme.caret,
            );
        } else {
            detail.draw_text(
                canvas,
                "filter...",
                inner_x,
                text_y,
                theme.muted,
                theme.filter_bg,
            );
        }
    }

    /// Draw the places sidebar: section headings, places, bookmarks, and the
    /// storage footer pinned to the bottom.
    ///
    /// The column runs the *full* height of the window -- past the toolbar's
    /// top and the status strip's bottom -- because in the guide's shell
    /// those bands belong to the right column alone.
    fn paint_sidebar(&mut self, canvas: &mut Canvas<'_>) {
        let Some(card) = self.card() else {
            return;
        };
        let theme = self.theme;
        let body = card.places();
        let top = body.y;
        let left = body.x;
        let width = body.w;
        let active = self.sidebar.index_of(self.browser.path());
        let row_h = theme.sidebar_row();
        let rows: Vec<SidebarRow> = self.sidebar.rows().to_vec();
        // Clamp here as well as at the wheel: rows can disappear (a device
        // unplugged) while the offset is stale, and painting must slice
        // rather than trust it.
        let visible = (body.h / row_h).max(1);
        let max = rows
            .len()
            .saturating_sub(usize::try_from(visible).unwrap_or(1));
        let start = usize::try_from(self.places_scroll).unwrap_or(0).min(max);
        for (i, row) in rows.iter().enumerate().skip(start) {
            let y =
                top + (i32::try_from(i).unwrap_or(0) - i32::try_from(start).unwrap_or(0)) * row_h;
            // Stop at the storage footer rather than painting through it.
            if y + row_h > body.y + body.h {
                break;
            }
            match row {
                SidebarRow::Heading(text) => {
                    // Headings sit in the small label tier and the muted
                    // colour, so they read as structure rather than as
                    // another place you could click. Their inset matches the
                    // row labels below them, so the section title and its
                    // entries share a left edge.
                    let label = &mut self.type_scale.label;
                    let ty = y + (row_h - label.line_height()) / 2;
                    label.draw_text(
                        canvas,
                        text,
                        left + Self::sidebar_text_x(&theme),
                        ty,
                        theme.muted,
                        theme.sidebar_bg,
                    );
                }
                SidebarRow::Hint(text) => {
                    let detail = &mut self.type_scale.detail;
                    let ty = y + (row_h - detail.line_height()) / 2;
                    detail.draw_text(
                        canvas,
                        text,
                        left + Self::sidebar_text_x(&theme),
                        ty,
                        theme.muted,
                        theme.sidebar_bg,
                    );
                }
                SidebarRow::Place { label, .. }
                | SidebarRow::Device { label, .. }
                | SidebarRow::Unmounted { label, .. } => {
                    self.paint_sidebar_entry(
                        canvas,
                        row,
                        label,
                        SidebarEntry {
                            x: left,
                            y,
                            width,
                            row_h,
                            active: active == Some(i),
                            hovered: self.hover.is(Pane::Sidebar, i),
                        },
                    );
                }
            }
        }
    }

    /// Draw one navigable sidebar row: its highlight, its name, and -- for a
    /// device -- the filesystem caption at the right edge.
    ///
    /// Split from [`Self::paint_sidebar`] when the device caption pushed that
    /// function past readable length. Headings and hints stay there because
    /// they are two lines each; this is the part with real branching.
    fn paint_sidebar_entry(
        &mut self,
        canvas: &mut Canvas<'_>,
        row: &SidebarRow,
        label: &str,
        at: SidebarEntry,
    ) {
        let theme = self.theme;
        let pad = theme.spacing.sidebar_pad();
        // Active outranks hover: the row marking where you *are* must not
        // lose its treatment just because the pointer happens to be resting
        // on it. Hover is a preview of a place you could go, so it says
        // nothing on the place you already went.
        let fill = match (at.active, at.hovered) {
            (true, _) => Some(theme.sidebar_active),
            (false, true) => Some(theme.hover),
            (false, false) => None,
        };
        let bg = fill.map_or(theme.sidebar_bg, |fill| {
            // The highlight is inset from both column edges and rounded, so
            // it reads as a pill behind the row rather than as a stripe
            // bleeding into the divider. Hover shares the geometry exactly,
            // so the fill appears to change colour under the pointer rather
            // than to also change shape.
            let pill = Rect::new(at.x + pad, at.y, (at.width - pad * 2).max(0), at.row_h);
            canvas.fill_round_rect(pill, fill, theme.spacing.radius());
            if at.active {
                // The guide's 2px accent rule on the leading edge, which is
                // what makes the active place readable at a glance even when
                // the fill is subtle. Kept inside the pill's straight middle
                // section so it does not poke out of the rounded corners.
                //
                // Deliberately not drawn for hover: the rule is the marker
                // for "you are here", and lending it to a transient pointer
                // position would make the pane appear to have two active
                // places.
                let r = theme.spacing.radius();
                canvas.fill_rect(
                    Rect::new(
                        at.x + pad,
                        at.y + r,
                        theme.spacing.rule(),
                        (at.row_h - r * 2).max(1),
                    ),
                    theme.accent,
                );
            }
            fill
        });
        // The open directory is drawn in the accent; the rest in ordinary
        // text, so exactly one row draws the eye. An unmounted device is
        // dimmed: it is a place that does not exist yet, and drawing it as
        // live as a browsable one invites a click that cannot navigate.
        let color = match (at.active, row.mountable_source().is_some()) {
            (true, _) => theme.accent,
            (false, true) => theme.disabled,
            (false, false) => theme.text,
        };
        let inset = at.x + Self::sidebar_text_x(&theme);
        let mut room = at.width - Self::sidebar_text_x(&theme) - pad;
        // A device's filesystem sits at the row's right edge in the muted
        // caption tier, so "vfat" or "read-only" is available without
        // competing with the name. The name is measured against what is left,
        // so the detail truncates the label rather than overprinting it.
        if let Some(detail) = row.detail() {
            let tier = &mut self.type_scale.detail;
            let detail_w = tier.measure(detail);
            // Only shown when the name keeps the larger share: below that the
            // row reads as two competing labels, and the name is the part
            // that identifies the disk.
            if detail_w * 2 < room {
                let ty = at.y + (at.row_h - tier.line_height()) / 2;
                tier.draw_text(canvas, detail, inset + room - detail_w, ty, theme.muted, bg);
                room -= detail_w + theme.spacing.s();
            }
        }
        let tier = &mut self.type_scale.body;
        let ty = at.y + (at.row_h - tier.line_height()) / 2;
        let shown = RowFormat::truncate(label, Self::columns_for(tier, room));
        tier.draw_text(canvas, &shown, inset, ty, color, bg);
    }

    /// Where text in the sidebar begins: the column's own padding plus the
    /// row's internal padding.
    ///
    /// Both are needed, and separately: the row highlight is inset by the
    /// column padding, and the label sits inside *that*. Collapsing the two
    /// into one number is what made the labels sit flush against the pane's
    /// edge.
    fn sidebar_text_x(theme: &Theme) -> i32 {
        theme.spacing.sidebar_pad() + theme.spacing.sidebar_row_pad()
    }

    /// Draw the sidebar's storage footer: the filesystem, how full it is, and
    /// a meter.
    ///
    /// Ambient information, which sets its whole treatment: the smallest type
    /// tier, muted colour, and a 2px bar rather than a chunky progress
    /// widget. When there is nothing meaningful to report -- a pseudo-
    /// filesystem, an unreadable path -- the footer is simply absent rather
    /// than showing a zeroed meter, which would look like a full disk's
    /// opposite rather than like "no data".
    fn paint_storage(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let Some(area) = self.card().and_then(|card| card.storage()) else {
            return;
        };
        let Some(storage) = self.storage.clone() else {
            return;
        };
        // A rule above the footer separates it from the places it sits under,
        // mirroring the guide's `border-top`.
        canvas.fill_rect(
            Rect::new(area.x, area.y, area.w, theme.spacing.hairline()),
            theme.sidebar_border,
        );

        // The footer's own padding matches the rows above it, so the device
        // name lines up with the place labels rather than sitting proud of
        // them.
        let inset = Self::sidebar_text_x(&theme);
        let usage = storage.usage();
        // "61% of 128g" when there is room for it; the bare percentage when
        // the sidebar is too narrow. The percentage is the part that must
        // survive, so it is what the fallback keeps.
        let percent = usage.summary();
        let top = area.y + theme.spacing.m();
        let room = area.w - inset * 2;
        if room <= 0 {
            return;
        }

        // The percentage is placed first and the device name gets what is
        // left: a long mount path may be truncated, but the number -- the
        // reason to look here at all -- is never the thing that is lost.
        let detail = &mut self.type_scale.detail;
        // Half the footer is the most the readout may claim before the device
        // name it shares the line with becomes unreadable.
        let percent = if detail.measure(&percent) * 2 > room {
            usage.percent_label()
        } else {
            percent
        };
        let percent_w = detail.measure(&percent);
        detail.draw_text(
            canvas,
            &percent,
            area.x + area.w - inset - percent_w,
            top,
            theme.muted,
            theme.sidebar_bg,
        );
        let name_room = room - percent_w - theme.spacing.s();
        let shown = RowFormat::truncate(storage.device(), Self::columns_for(detail, name_room));
        detail.draw_text(
            canvas,
            &shown,
            area.x + inset,
            top,
            theme.muted,
            theme.sidebar_bg,
        );

        // The meter: a full-width track with the used portion in the accent,
        // so "how full" is legible without reading the number.
        let track_h = theme.spacing.meter_height();
        let track_y = top + self.type_scale.detail.line_height() + theme.spacing.s();
        // Only drawn if it actually fits inside the footer -- a bar clipped by
        // the window's bottom edge would misreport the fill level.
        if track_y + track_h > area.bottom() {
            return;
        }
        // Rounded to its own half-height, so a 2px meter reads as a soft rule
        // rather than a hard tick. `RoundRect` clamps the radius, so this stays
        // correct if the meter ever grows.
        let cap = track_h / 2;
        canvas.fill_round_rect(
            Rect::new(area.x + inset, track_y, room, track_h),
            theme.scrollbar_track,
            cap,
        );
        let filled = usage.meter_width(room);
        if filled > 0 {
            canvas.fill_round_rect(
                Rect::new(area.x + inset, track_y, filled, track_h),
                theme.accent,
                cap,
            );
        }
    }

    /// How many characters of `text` fit in `width` pixels for a given tier.
    ///
    /// The bitmap font was fixed-width, so the old code could divide by a
    /// constant glyph width. A real face cannot: this measures the tier's own
    /// average advance instead of assuming one.
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

    /// Draw the status line: the filter box, or a summary.
    fn paint_status(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let Some(band) = self.card().map(|card| card.status()) else {
            return;
        };
        if band.h == 0 {
            return;
        }
        let y = band.y;
        let background = theme.sidebar_bg;
        canvas.fill_rect(
            Rect::new(band.x, y, band.w, theme.spacing.hairline()),
            theme.sidebar_border,
        );

        // An error outranks everything else: it explains why the list is not
        // what the user expected, and hiding it behind a file count would
        // leave them staring at a stale listing wondering what happened.
        if let Some(error) = self.browser.error() {
            let message = error.message();
            let detail = &mut self.type_scale.detail;
            let text_y = y + (band.h - detail.line_height()) / 2;
            detail.draw_text(
                canvas,
                &message,
                band.x + theme.spacing.m(),
                text_y,
                theme.error,
                background,
            );
            return;
        }

        // The guide's status line reports the entry count or the selection.
        // The filter query itself now lives in the toolbar control, so the
        // strip stays a summary rather than duplicating it.
        //
        // A file operation's result outranks the count while it is current:
        // "3 items copied" is what the user just did, and the entry count is
        // still there beneath it on the next action.
        let shown = self.browser.len();
        let total = self.browser.total();
        let summary = self.browser.notice().map_or_else(
            || {
                if shown == total {
                    format!("{total} entries")
                } else {
                    format!("{shown} of {total} entries")
                }
            },
            ToOwned::to_owned,
        );
        let label = &mut self.type_scale.label;
        let text_y = y + (band.h - label.line_height()) / 2;
        label.draw_text(
            canvas,
            &summary,
            band.x + theme.spacing.m(),
            text_y,
            theme.muted,
            background,
        );

        self.paint_status_hint(canvas, band, background);
    }

    /// Draw the keyboard hint at the status line's right end.
    ///
    /// The guide gives the hint a minimal keycap: an outlined box around the
    /// chord with its description beside it in muted text.
    fn paint_status_hint(&mut self, canvas: &mut Canvas<'_>, band: Rect, background: Color) {
        let y = band.y;
        let theme = self.theme;
        // Which hint earns the space depends on what the user can do next:
        // while filtering, escaping is what they will want.
        let (description, chord) = if self.browser.is_filtering() {
            ("clear filter", "esc")
        } else {
            ("select all", "ctrl+a")
        };

        let label = &mut self.type_scale.label;
        let pad = theme.spacing.xs();
        let chord_w = label.measure(chord) + pad * 2;
        let cap_h = label.line_height() + pad;
        let gap = theme.spacing.s();
        let total = label.measure(description) + gap + chord_w;
        let start = band.right() - theme.spacing.m() - total;
        // A column too narrow for the hint drops it rather than overlapping
        // the entry count, which is the more useful of the two.
        let count_end = band.x + theme.spacing.m() + label.measure("9999 of 9999 entries");
        if start < count_end {
            return;
        }

        let text_y = y + (band.h - label.line_height()) / 2;
        let after = label.draw_text(canvas, description, start, text_y, theme.muted, background);
        let cap_x = after + gap;
        let cap_y = y + (band.h - cap_h) / 2;
        canvas.draw_round_rect(
            Rect::new(cap_x, cap_y, chord_w, cap_h),
            theme.control_border,
            theme.spacing.radius_small(),
        );
        let label = &mut self.type_scale.label;
        label.draw_text(canvas, chord, cap_x + pad, text_y, theme.muted, background);
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
        // Last, and over everything: the badge is cargo the user is holding
        // above the window, so anything it covers is by definition behind it.
        self.paint_drag_badge(canvas);
        // Later still: the modal covers the badge too. While it is up nothing
        // behind it is usable, so nothing behind it may be drawn on top.
        self.paint_password(canvas);
    }

    fn on_key(&mut self, press: KeyPress) {
        self.sync_tiles();
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
            // A secondary click on a device flushes it. The one honest
            // removal step available without privileges: `umount(2)` needs
            // CAP_SYS_ADMIN, so "eject" is not ours to offer, but pushing
            // pending writes to the hardware is, and it is the part that
            // actually protects the user's data.
            PointerEventKind::Press {
                button: PointerButton::Right,
                at,
                ..
            } => {
                if self.in_sidebar(at)
                    && let Some(index) = self.sidebar_row_at(at)
                {
                    self.flush_device(index);
                }
            }
            PointerEventKind::Release { .. } => {
                self.drag_arm.cancel();
                self.drag_row = None;
                self.release_tile();
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
                    self.tiles.disarm();
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
                if self.in_sidebar(at) {
                    self.scroll_places(lines);
                    return;
                }
                let (spiral, tail) = self.flow_parts();
                if self
                    .tiles
                    .wheel(spiral, self.theme.spacing.tile_min(), tail, lines)
                {
                    let spot = self.spot_at(at);
                    let _ = self.hover.move_to(spot);
                }
            }
            // The pointer left the surface, so the highlight must go with it;
            // otherwise a row stays lit while the pointer is in another window.
            PointerEventKind::Leave => {
                let _ = self.hover.clear();
                self.drag_arm.cancel();
                self.drag_row = None;
                self.tiles.disarm();
            }
            // Enter carries a position, but the Motion event that follows it
            // does the hover work -- handling it here too would only duplicate
            // that. A non-left press stays deliberately inert until there is a
            // context menu to open.
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
    use super::{FilesWindow, Painter, PointerEventKind, Target};
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
        let theme = crate::theme::Theme::from_config(&cfg);
        let type_scale = crate::typography::Typography::load(&cfg, 1.0);
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
        let theme = crate::theme::Theme::from_config(&cfg);
        let type_scale = crate::typography::Typography::load(&cfg, 1.0);
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
        let theme = crate::theme::Theme::from_config(&cfg);
        let type_scale = crate::typography::Typography::load(&cfg, 1.0);
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
            Target::ToggleHidden => bar.hidden_toggle(),
            Target::FilterBox => bar.filter(),
            Target::Up => bar.up(),
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

    /// Give `painter` a sidebar with `volumes` attached, as the poller does
    /// when a device appears.
    fn attach_devices(painter: &mut FilesWindow, volumes: &volumes::VolumeSet) {
        let dirs = user_dirs::UserDirs::load();
        painter.sidebar = crate::sidebar::Sidebar::with_volumes(&dirs, &painter.favorites, volumes);
        painter.scroll_places(i32::MAX / 2);
    }

    /// A USB stick attached at `/run/media/user/<label>`.
    fn attached_stick(label: &str) -> volumes::VolumeSet {
        volumes::VolumeSet::of(vec![volumes::Volume::new(
            label,
            format!("/run/media/user/{label}"),
            "/dev/sdb1",
            "vfat",
            volumes::DeviceClass::Removable,
            false,
        )])
    }

    /// The centre of sidebar row `index`, as a pointer position.
    fn sidebar_row_centre(painter: &FilesWindow, index: usize) -> super::Position {
        let places = painter.card().expect("the card is shown").places();
        let row_h = painter.theme.sidebar_row();
        super::Position {
            x: places.x + places.w / 2,
            y: places.y
                + (i32::try_from(index).expect("a small index") - painter.places_scroll) * row_h
                + row_h / 2,
        }
    }

    /// A device row must actually reach the canvas, not merely exist in the
    /// model. The section is drawn below the existing rows, so a painter that
    /// stopped at the old row count would leave it invisible.
    #[test]
    fn an_attached_device_changes_the_painted_sidebar() {
        let (mut painter, _root, size) = painter_over_a_mixed_directory("dev-paint");
        let before = paint_opaque(&mut painter, size);
        attach_devices(&mut painter, &attached_stick("KINGSTON"));
        let after = paint_opaque(&mut painter, size);
        assert_ne!(
            before, after,
            "the Devices section must be visible once a stick is attached"
        );
    }

    /// Clicking a device row navigates to its mount point like any place.
    #[test]
    fn clicking_a_device_row_navigates_to_its_mount_point() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("dev-click");
        // A real directory standing in for a mount point, so the navigation
        // can succeed -- this machine has no removable media to point at.
        let mount = root.join("a_directory");
        attach_devices(
            &mut painter,
            &volumes::VolumeSet::of(vec![volumes::Volume::new(
                "KINGSTON",
                mount.clone(),
                "/dev/sdb1",
                "vfat",
                volumes::DeviceClass::Removable,
                false,
            )]),
        );
        let index = painter
            .sidebar
            .rows()
            .iter()
            .position(crate::sidebar::SidebarRow::is_device)
            .expect("the stick");
        let at = sidebar_row_centre(&painter, index);
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
        assert_eq!(painter.browser.path(), mount.as_path());
    }

    /// The guard that matters: a secondary click on an ordinary place must
    /// not be treated as a device, or the flush would act on the user's home
    /// directory or the root filesystem.
    #[test]
    fn a_secondary_click_on_a_plain_place_flushes_nothing() {
        let (painter, _root, _size) = painter_over_a_mixed_directory("dev-guard");
        for (index, row) in painter.sidebar.rows().iter().enumerate() {
            if !row.is_device() {
                assert!(
                    !painter.flush_device(index),
                    "{:?} is not a device and must not be flushed",
                    row.path()
                );
            }
        }
    }

    /// And the positive case: a device row does flush. The mount point is a
    /// real directory, so `syncfs` succeeds and the call reports it acted.
    #[test]
    fn a_secondary_click_on_a_device_flushes_it() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("dev-flush");
        attach_devices(
            &mut painter,
            &volumes::VolumeSet::of(vec![volumes::Volume::new(
                "KINGSTON",
                root.join("a_directory"),
                "/dev/sdb1",
                "vfat",
                volumes::DeviceClass::Removable,
                false,
            )]),
        );
        let index = painter
            .sidebar
            .rows()
            .iter()
            .position(crate::sidebar::SidebarRow::is_device)
            .expect("the stick");
        assert!(painter.flush_device(index), "a device row flushes");
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
        assert!(
            !painter.hover.is(Pane::Tiles, 0),
            "and so no row may be lit"
        );
    }

    /// Hovering a row must actually change what gets painted.
    ///
    /// Compares whole frames rather than probing a pixel: the highlight's exact
    /// position depends on the frame geometry, and a test that hardcodes a
    /// coordinate breaks every time the layout is tuned -- which is exactly
    /// what this change did to the row height.
    #[test]
    fn hovering_a_row_changes_the_painted_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-frame");
        let cold = paint_opaque(&mut painter, size);

        assert!(
            painter.hover.move_to(Some(Spot::Item(Pane::Tiles, 0))),
            "row 0 becomes hovered"
        );
        let hot = paint_opaque(&mut painter, size);
        assert_ne!(cold, hot, "a hovered row must be visibly highlighted");

        // ...and clearing it must restore the original frame exactly, or the
        // highlight leaks and rows stay lit after the pointer leaves.
        assert!(painter.hover.clear(), "the hover is dropped");
        let cleared = paint_opaque(&mut painter, size);
        assert_eq!(cold, cleared, "clearing hover must restore the frame");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The sidebar's places must track the pointer too, not just the list.
    ///
    /// Whole-frame comparison for the same reason as the list version: the
    /// pill's position falls out of the frame geometry, so a hardcoded
    /// coordinate would break on every layout tweak.
    #[test]
    fn hovering_a_sidebar_place_changes_the_painted_frame() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-sidebar");
        // Row 0 is the "Places" heading; row 1 is the first real place.
        let place = painter
            .sidebar
            .rows()
            .iter()
            .position(crate::sidebar::SidebarRow::selectable)
            .expect("the sidebar always offers at least Home");
        let cold = paint_opaque(&mut painter, size);

        assert!(
            painter
                .hover
                .move_to(Some(Spot::Item(Pane::Sidebar, place)))
        );
        let hot = paint_opaque(&mut painter, size);
        assert_ne!(cold, hot, "a hovered place must be visibly highlighted");

        assert!(painter.hover.clear());
        let cleared = paint_opaque(&mut painter, size);
        assert_eq!(cold, cleared, "clearing hover must restore the frame");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A heading is not a destination, so it must not light up either --
    /// otherwise the pane appears to offer somewhere to go that does nothing
    /// when clicked.
    #[test]
    fn hovering_a_sidebar_heading_highlights_nothing() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-heading");
        let heading = painter
            .sidebar
            .rows()
            .iter()
            .position(|r| !r.selectable())
            .expect("the pane opens with a Places heading");
        let cold = paint_opaque(&mut painter, size);

        // The hover tracker refuses the row outright, so the frame cannot
        // change: `row_index_at` filters unselectable rows for both paths.
        let at = sidebar_row_centre(&painter, heading);
        assert_eq!(
            painter.sidebar_row_at(at),
            None,
            "a heading resolves to no row"
        );
        let hot = paint_opaque(&mut painter, size);
        assert_eq!(cold, hot, "a heading must not paint a highlight");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The row marking where you *are* must keep its own treatment while the
    /// pointer rests on it: hover is a preview of somewhere you could go, so it
    /// has nothing to say about the place you already went.
    #[test]
    fn the_active_place_outranks_its_own_hover() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-active");
        // Navigate to a place the sidebar actually lists, so one row is active.
        let (index, path) = painter
            .sidebar
            .rows()
            .iter()
            .enumerate()
            .find_map(|(i, r)| r.path().map(|p| (i, p.to_path_buf())))
            .expect("a listed place");
        let outcome = painter.browser.go_to(&path);
        painter.apply(outcome);

        let resting = paint_opaque(&mut painter, size);
        assert!(
            painter
                .hover
                .move_to(Some(Spot::Item(Pane::Sidebar, index)))
        );
        let hovered = paint_opaque(&mut painter, size);
        assert_eq!(
            resting, hovered,
            "hovering the active place must not restyle it"
        );
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

    /// A directory and a file must not get the same highlight -- the whole
    /// point of the per-type fills, and the property most at risk of quietly
    /// collapsing if both roles are ever derived from the same colour.
    #[test]
    fn a_directory_and_a_file_highlight_differently() {
        let (painter, root, _) = painter_over_a_mixed_directory("hover-theme");
        let theme = &painter.theme;
        assert_ne!(
            theme.hover_dir, theme.hover_file,
            "hover must distinguish a directory from a file"
        );
        assert_ne!(
            theme.selection_dir, theme.selection_file,
            "selection must distinguish a directory from a file"
        );
        // Selection has to outrank hover, or selecting a row the pointer is
        // over would appear to do nothing.
        assert_ne!(theme.selection_dir, theme.hover_dir);
        assert_ne!(theme.selection_file, theme.hover_file);
        // Every highlight must differ from the plain row background too, or it
        // is invisible rather than subtle.
        for fill in [
            theme.hover_dir,
            theme.hover_file,
            theme.selection_dir,
            theme.selection_file,
        ] {
            assert_ne!(fill, theme.background, "a highlight must be visible");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Hovering the two kinds of row must paint *differently* from each other,
    /// not merely differ in the theme. This is the end-to-end version: it
    /// proves the painter actually consults the row's kind rather than reaching
    /// for one fill for everything.
    #[test]
    fn the_painted_highlight_follows_the_rows_kind() {
        let (mut painter, root, size) = painter_over_a_mixed_directory("hover-kind");
        // Sorted with directories first, so row 0 is the directory and row 1
        // the file. Asserted rather than assumed -- if the sort order ever
        // changes, this test must fail loudly instead of silently comparing
        // two files.
        assert!(
            painter
                .browser
                .entry(0)
                .is_some_and(fsmodel::entry::Entry::is_navigable),
            "row 0 must be the directory"
        );
        assert!(
            painter.browser.entry(1).is_some_and(|e| !e.is_navigable()),
            "row 1 must be the file"
        );

        painter.hover.move_to(Some(Spot::Item(Pane::Tiles, 0)));
        let dir_hovered = paint_opaque(&mut painter, size);
        painter.hover.move_to(Some(Spot::Item(Pane::Tiles, 1)));
        let file_hovered = paint_opaque(&mut painter, size);
        assert_ne!(
            dir_hovered, file_hovered,
            "the two rows must not receive the same highlight"
        );
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
                placed
                    .tile
                    .item()
                    .and_then(|row| painter.browser.entry(row))
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
    fn clicking_a_folder_tile_enters_the_folder() {
        let (mut painter, root, _size) = tiled("tiles-enter");
        let at = tile_centre(&painter, "a_directory");
        left_press(&mut painter, at);
        assert_eq!(
            painter.browser.path(),
            root,
            "a press alone must not navigate"
        );
        left_release(&mut painter, at);
        assert_eq!(painter.browser.path(), root.join("a_directory"));
        assert!(painter.browser.can_go_back(), "the step is in the history");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_double_click_on_a_folder_tile_does_not_also_hit_the_next_tile() {
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
        assert_ne!(paint_opaque(&mut painter, size), resting);
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
            crate::theme::Theme::from_config(&cfg),
            crate::typography::Typography::load(&cfg, 1.0),
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

    #[test]
    fn the_overflow_tile_opens_a_page_and_up_closes_it() {
        let (mut painter, root) = crowded("tiles-overflow", 60);
        let more = painter
            .tile_layout()
            .into_iter()
            .find(|placed| placed.tile.item().is_none())
            .expect("sixty files overflow a 900x600 window");
        let at = super::Position {
            x: more.rect.x + more.rect.w / 2,
            y: more.rect.y + more.rect.h / 2,
        };
        left_press(&mut painter, at);
        left_release(&mut painter, at);
        assert_eq!(painter.tiles.depth(), 1);
        assert!(painter.can_step_out());
        click_control(&mut painter, Target::Up);
        assert_eq!(painter.tiles.depth(), 0);
        assert_eq!(
            painter.browser.path(),
            root,
            "up closed the page, not the folder"
        );
        let _ = std::fs::remove_dir_all(&root);
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
    fn walking_off_the_last_tile_opens_the_overflow_page() {
        let (mut painter, root) = crowded("tiles-keys-overflow", 60);
        key(&mut painter, platform::toplevel::Key::End, false);
        let last = cursor_name(&painter);
        key(&mut painter, platform::toplevel::Key::Down, false);
        assert_eq!(painter.tiles.depth(), 1);
        assert_ne!(cursor_name(&painter), last);
        key(&mut painter, platform::toplevel::Key::Up, false);
        assert_eq!(painter.tiles.depth(), 0);
        assert_eq!(
            cursor_name(&painter),
            last,
            "walking back lands where it left"
        );
        key(&mut painter, platform::toplevel::Key::PageDown, false);
        assert_eq!(painter.tiles.depth(), 1);
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
    fn the_wheel_turns_tile_pages() {
        let (mut painter, root) = crowded("tiles-wheel", 60);
        wheel(&mut painter, -3);
        assert_eq!(painter.tiles.depth(), 0);
        wheel(&mut painter, 3);
        assert_eq!(painter.tiles.depth(), 1);
        wheel(&mut painter, -3);
        assert_eq!(painter.tiles.depth(), 0);
        assert_eq!(
            painter.browser.path(),
            root,
            "the wheel never leaves the folder"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_go_up_key_closes_an_overflow_page_first() {
        let (mut painter, root) = crowded("tiles-overflow-key", 60);
        let from = painter
            .tile_layout()
            .into_iter()
            .find_map(|placed| match placed.tile {
                crate::tile_rank::Tile::More { from, .. } => Some(from),
                crate::tile_rank::Tile::Item(_) => None,
            })
            .expect("overflow");
        painter.tiles.open_more(from);
        painter.on_key(platform::toplevel::KeyPress {
            key: platform::toplevel::Key::Backspace,
            modifiers: platform::toplevel::ModifierState::default(),
            repeat: false,
        });
        assert_eq!(painter.tiles.depth(), 0);
        assert_eq!(painter.browser.path(), root);
        let _ = std::fs::remove_dir_all(&root);
    }
}
