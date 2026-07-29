//! [`FilesWindow`] -- the [`Painter`] that draws a [`Browser`] and routes
//! compositor input into it.
//!
//! This is the only module in the binary that knows about pixels or Wayland,
//! and it holds no application state of its own beyond what drawing needs:
//! every decision it makes was already made by `Browser`. That is the payoff
//! of the layering -- the file-manager behaviour is tested headlessly, and
//! what is left here is a straight transcription onto a canvas.

use platform::draw::{Canvas, Color, Rect};
use platform::protocol::PointerButton;
use platform::toplevel::{KeyPress, Painter, PhysicalSize, PointerEventKind, Position};
use widget::{DragArm, Mods};

use crate::breadcrumb::Breadcrumb;
use crate::browser::{Browser, Outcome, ViewMode};
use crate::columns::{Column, Columns};
use crate::favorites::Favorites;
use crate::frame::Frame;
use crate::hover::{Hover, Pane, Spot};
use crate::kind_label::KindLabel;
use crate::moment::Moment;
use crate::row_format::RowFormat;
use crate::sidebar::{Sidebar, SidebarRow};
use crate::storage::Storage;
use crate::theme::Theme;
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

/// Which family of highlight colours a row draws from.
///
/// A two-variant enum rather than the `navigable: bool` it replaced, because
/// `bool` at a call site reads as "true means... what, exactly?" -- and because
/// this is the axis most likely to gain a third case (a symlink family, say),
/// which a bool cannot express without every call site changing shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    /// Anything Enter would descend into -- a directory, or a symlink to one.
    /// The same test `RowFormat::name` uses for its trailing slash, so the
    /// highlight tint and the slash cannot disagree about what a row is.
    Navigable,
    /// A leaf: a regular file, a device, a broken link.
    Leaf,
}

impl RowKind {
    /// Classify an entry.
    const fn of(entry: &fsmodel::entry::Entry) -> Self {
        if entry.is_navigable() {
            Self::Navigable
        } else {
            Self::Leaf
        }
    }

    /// The selection fill for this kind.
    ///
    /// The two lookups sit side by side deliberately: a mismatched pair (hover
    /// reading the directory colour while selection reads the file one) is
    /// obvious here and invisible if they live at separate call sites.
    const fn selection_fill(self, theme: &Theme) -> Color {
        match self {
            Self::Navigable => theme.selection_dir,
            Self::Leaf => theme.selection_file,
        }
    }

    /// The hover fill for this kind.
    const fn hover_fill(self, theme: &Theme) -> Color {
        match self {
            Self::Navigable => theme.hover_dir,
            Self::Leaf => theme.hover_file,
        }
    }
}

/// One row's strings and states, gathered before drawing.
///
/// A named struct rather than a tuple because six positional fields of which
/// two are `bool` is exactly the shape that silently swaps `selected` and
/// `is_cursor` during a refactor.
#[allow(clippy::struct_excessive_bools)] // each flag is an independent, unrelated painted state, not a state machine
struct PaintedRow {
    y: i32,
    name: String,
    kind: String,
    modified: String,
    size: String,
    color: Color,
    selected: bool,
    is_cursor: bool,
    hovered: bool,
    /// Whether an incoming drag is currently hovering this row -- painted
    /// as an outline so it reads distinctly from selection/hover fills
    /// (which mean "this is what you've chosen" or "this is under the
    /// pointer", not "dropping here will move files into this folder").
    drop_target: bool,
    /// Which highlight family this row draws from.
    row_kind: RowKind,
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
}

impl FilesWindow {
    #[must_use]
    pub const fn new(
        browser: Browser,
        theme: Theme,
        type_scale: Typography,
        sidebar: Sidebar,
        favorites: Favorites,
        size: PhysicalSize,
    ) -> Self {
        Self {
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
        self.drop_hover_row = if over {
            let rows_top = self.frame().rows().y;
            let rows_left = self.frame().rows().x;
            self.browser
                .drop_target_row(at.0 - rows_left, at.1 - rows_top)
        } else {
            None
        };
    }

    /// Re-read the filesystem figures behind the sidebar footer.
    ///
    /// Called on navigation rather than per paint: crossing into a different
    /// mount is the only thing that can change which filesystem is being
    /// reported, and the fill level moves far slower than the frame rate.
    fn refresh_storage(&mut self) {
        self.storage = Storage::of(self.browser.path());
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

    /// Show or hide the places pane, re-laying out the list to match.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar.toggle();
        self.relayout();
    }

    /// Rebuild the sidebar rows after the favorites list changed.
    fn rebuild_sidebar(&mut self) {
        let dirs = crate::user_dirs::UserDirs::load();
        let visible = self.sidebar.visible();
        self.sidebar = Sidebar::new(&dirs, &self.favorites);
        if !visible {
            self.sidebar.toggle();
        }
    }

    /// Tell the browser how much room the list actually has.
    ///
    /// Factored out of `resize` because toggling the sidebar changes the
    /// list's width without any window resize -- and forgetting to re-layout
    /// there would leave the list hit-testing against a stale width.
    fn relayout(&mut self) {
        let rows = self.frame().rows();
        self.browser.resize(rows.w.max(0), rows.h.max(0));
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
            self.theme.header_height,
            self.theme.status_height,
            self.theme.column_header(),
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

    /// Tell the browser about a new window size.
    pub fn resize(&mut self, size: PhysicalSize) {
        self.size = size;
        self.relayout();
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
            Outcome::Ignored => {}
        }
    }

    /// Convert a pointer position into list-pane coordinates.
    ///
    /// Returns `None` above or below the list, so a click on the header or
    /// status bar cannot be mistaken for a click on whichever row happens to
    /// line up with it -- and `None` over the sidebar, so a click on a place
    /// does not also land on the row beside it.
    fn to_pane(&self, at: Position) -> Option<(i32, i32)> {
        let rows = self.frame().rows();
        let y = at.y - rows.y;
        if y < 0 || y >= rows.h {
            return None;
        }
        let x = at.x - rows.x;
        if x < 0 || x >= rows.w {
            return None;
        }
        Some((x, y))
    }

    /// Whether `at` falls inside the places pane's clickable rows.
    ///
    /// The sidebar now runs the full height of the window, so this is the
    /// whole column *except* the storage footer -- which is a readout, not a
    /// control, and must not navigate anywhere when clicked.
    fn in_sidebar(&self, at: Position) -> bool {
        let frame = self.frame();
        let body = frame.sidebar_body();
        frame.has_sidebar() && at.x < body.w && at.y >= body.y && at.y < body.y + body.h
    }

    /// Which sidebar row a pointer position falls on, if any.
    ///
    /// Shared by the press handler and the hover tracker deliberately: they
    /// must agree, and two copies of this arithmetic would be free to drift by
    /// a row -- lighting one place while clicking another, which reads as a
    /// rendering bug rather than a logic one. It also has to match where
    /// `paint_sidebar` starts drawing, hence the same top inset.
    /// Which toolbar control is under `at`, if any.
    fn toolbar_hit(&self, at: Position) -> Option<Target> {
        self.toolbar().hit(at.x, at.y)
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
            return self.sidebar_row_at(at).map(|i| Spot::Row(Pane::Sidebar, i));
        }
        self.to_pane(at)
            .and_then(|(x, y)| match self.browser.view_mode() {
                ViewMode::List => self.browser.pane().viewport().row_at_y(y),
                ViewMode::Grid => self.browser.grid().item_at(x, y),
            })
            .map(|i| Spot::Row(Pane::List, i))
    }

    /// Act on a click on toolbar control `target`.
    fn activate_control(&mut self, target: Target) {
        let outcome = match target {
            Target::Back => self.browser.go_back(),
            Target::Forward => self.browser.go_forward(),
            Target::Filter => self.browser.focus_filter(),
            Target::ViewList => self.browser.set_view_mode(ViewMode::List),
            Target::ViewGrid => self.browser.set_view_mode(ViewMode::Grid),
        };
        self.apply(outcome);
        // Back and forward can cross a filesystem boundary, exactly as a
        // sidebar jump can, so the storage footer must be re-read.
        if matches!(target, Target::Back | Target::Forward) {
            self.refresh_storage();
        }
    }

    fn sidebar_row_at(&self, at: Position) -> Option<usize> {
        let y = at.y - self.frame().sidebar_body().y - self.theme.spacing.sidebar_pad_y();
        self.sidebar.row_index_at(y, self.theme.sidebar_row())
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

    /// Draw the toolbar: navigation affordances, the breadcrumb path, the
    /// filter control and the view toggle.
    ///
    /// Confined to the right column, per the guide's shell: the sidebar runs
    /// past it, top to bottom.
    fn paint_toolbar(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let bar = self.frame().toolbar();
        // Background band -- see `paint`'s note on glass versus paint.
        canvas.fill_rect_glass(bar, theme.toolbar_bg);
        // A hairline under the toolbar separates it from the list without
        // spending a whole row of vertical space on a gap.
        canvas.fill_rect(
            Rect::new(
                bar.x,
                bar.bottom() - theme.spacing.hairline(),
                bar.w,
                theme.spacing.hairline(),
            ),
            theme.sidebar_border,
        );
        self.paint_nav_buttons(canvas);
        self.paint_breadcrumb(canvas);
        self.paint_filter_control(canvas);
        self.paint_view_toggle(canvas);
    }

    /// The toolbar's control geometry for the current window.
    ///
    /// The single source of truth for where the buttons are, shared by the
    /// painter and the pointer handler so the clickable region cannot drift
    /// away from the drawn one.
    fn toolbar(&self) -> Toolbar {
        Toolbar::new(
            self.frame().toolbar(),
            self.theme.spacing,
            self.theme.control_height(),
            self.browser.is_filtering(),
        )
    }

    /// Draw the back and forward chevrons at the toolbar's left.
    ///
    /// Each is live only when the history actually holds a step in that
    /// direction, and is drawn muted when it does not -- a disabled control
    /// that looks enabled teaches the user the application is broken, and one
    /// that looks disabled but works is just as confusing. Hover is drawn only
    /// on a live button, for the same reason.
    fn paint_nav_buttons(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let bar = self.toolbar();
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
                theme.toolbar_bg
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

    /// Draw the list/grid view toggle at the toolbar's right end.
    ///
    /// Two live segments share an outline; the active presentation gets the
    /// accent wash, so the toggle states what the content is showing.
    fn paint_view_toggle(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        // Dropped entirely rather than overlapped when the toolbar is too
        // tight: the filter box and the path outrank it.
        let Some(outline) = self.toolbar().view_toggle() else {
            return;
        };
        let button = theme.spacing.view_button();
        let (x, y, width, height) = (outline.x, outline.y, outline.w, outline.h);
        // The active segment is filled, which is what makes the pair read as
        // a state rather than as two buttons. Only its *outer* corners are
        // rounded -- it shares a straight edge with the segment beside it, so
        // rounding all four would leave a notch mid-pair. Drawing the fill one
        // radius wider and squaring off that overhang gives the D-shape.
        let radius = theme.spacing.radius();
        let active = match self.browser.view_mode() {
            ViewMode::List => 0,
            ViewMode::Grid => 1,
        };
        let fill_x = x + button * active;
        canvas.fill_round_rect(
            Rect::new(
                fill_x - if active == 1 { radius } else { 0 },
                y,
                button + radius,
                height,
            ),
            theme.accent_surface,
            radius,
        );
        canvas.fill_rect(
            Rect::new(
                if active == 0 {
                    fill_x + button
                } else {
                    fill_x - radius
                },
                y,
                radius,
                height,
            ),
            theme.accent_surface,
        );
        canvas.draw_round_rect(Rect::new(x, y, width, height), theme.control_border, radius);
        for (i, glyph) in ["\u{2261}", "\u{22ee}"].into_iter().enumerate() {
            let bg = if i32::try_from(i).unwrap_or(-1) == active {
                theme.accent_surface
            } else {
                theme.toolbar_bg
            };
            let color = if i32::try_from(i).unwrap_or(-1) == active {
                theme.accent
            } else {
                theme.text
            };
            let tier = &mut self.type_scale.label;
            let text_w = tier.measure(glyph);
            let text_y = y + (height - tier.line_height()) / 2;
            let seg_x = x + button * i32::try_from(i).unwrap_or(0);
            tier.draw_text(
                canvas,
                glyph,
                seg_x + (button - text_w) / 2,
                text_y,
                color,
                bg,
            );
        }
    }

    /// Draw the breadcrumb path, most-significant segment first.
    ///
    /// The current segment is drawn in full-strength text and the ancestors
    /// muted, so the eye lands on where you *are* rather than on the path you
    /// took to get there.
    fn paint_breadcrumb(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let crumbs = self.crumbs();
        let bar = self.frame().toolbar();
        let toolbar = self.toolbar();
        let mut x = toolbar.breadcrumb_x();
        // The filter control and view toggle own the right end of the toolbar,
        // so the path gets what is left after them and their gaps.
        let limit = toolbar.breadcrumb_width();

        let (shown, elided) = crumbs.tail(self.crumb_budget(limit));
        if elided {
            let label = &mut self.type_scale.label;
            let y = label.baseline_y(bar.h) + bar.y;
            x = label.draw_text(
                canvas,
                Breadcrumb::ELLIPSIS,
                x,
                y,
                theme.muted,
                theme.toolbar_bg,
            );
            x += theme.spacing.xs();
        }

        for (i, crumb) in shown.iter().enumerate() {
            // A separator between segments, but never before the first: the
            // root crumb already reads as "/".
            if i > 0 || elided {
                let label = &mut self.type_scale.label;
                let y = label.baseline_y(bar.h) + bar.y;
                x = label.draw_text(canvas, "/", x, y, theme.muted, theme.toolbar_bg);
                x += theme.spacing.xs();
            }
            let (color, tier) = if crumb.current {
                (theme.text, &mut self.type_scale.title)
            } else {
                (theme.muted, &mut self.type_scale.body)
            };
            let y = tier.baseline_y(bar.h) + bar.y;
            x = tier.draw_text(canvas, &crumb.label, x, y, color, theme.toolbar_bg);
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
        let Some(rect) = self.toolbar().filter() else {
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

    /// Draw the column-header row: small, lowercase, letter-spaced labels
    /// over a hairline, per the guide.
    fn paint_column_headers(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let band = self.frame().column_headers();
        let (y, height, left, width) = (band.y, band.h, band.x, band.w);
        canvas.fill_rect_glass(band, theme.background);

        let columns = self.list_columns();
        for cell in columns.cells() {
            let heading = cell.column.heading();
            let label = &mut self.type_scale.label;
            let text_y = y + (height - label.line_height()) / 2;
            // Right-aligned headings sit over right-aligned data, so the
            // "size" label lines up with the digits beneath it.
            let text_x = cell.text_x(label.measure(heading));
            label.draw_text(
                canvas,
                heading,
                text_x,
                text_y,
                theme.muted,
                theme.background,
            );
        }

        // The rule under the headings, which is what makes them read as a
        // table header rather than as a first row.
        canvas.fill_rect(
            Rect::new(
                left,
                y + height - theme.spacing.hairline(),
                width,
                theme.spacing.hairline(),
            ),
            theme.sidebar_border,
        );
    }

    /// The column layout for the current window.
    ///
    /// Recomputed per paint rather than cached: it depends on the window
    /// width, the sidebar's visibility and the scrollbar's presence, and a
    /// stale copy would put the headings out of line with the data.
    fn list_columns(&self) -> Columns {
        let theme = self.theme;
        let frame = self.frame();
        // The rows reserve the scrollbar gutter, so the headers must too or
        // the "size" heading floats away from the numbers below it.
        let content = self.browser.pane().content_width();
        Columns::new(
            frame.content_x(),
            // The pane's own width, not the frame's: the rows reserve a
            // scrollbar gutter the frame does not know about, and the table
            // must stop short of it rather than run underneath.
            (content - frame.inset() * 2)
                .max(0)
                .min(frame.content_width()),
            theme.spacing.s(),
            theme.is_compact(self.size.width),
        )
    }

    /// Draw the places sidebar: section headings, places, bookmarks, and the
    /// storage footer pinned to the bottom.
    ///
    /// The column runs the *full* height of the window -- past the toolbar's
    /// top and the status strip's bottom -- because in the guide's shell
    /// those bands belong to the right column alone.
    fn paint_sidebar(&mut self, canvas: &mut Canvas<'_>) {
        let frame = self.frame();
        if !frame.has_sidebar() {
            return;
        }
        let theme = self.theme;
        let column = frame.sidebar();
        let body = frame.sidebar_body();
        // The places list is inset from the top by the guide's own sidebar
        // padding, so the first row does not collide with the toolbar's
        // baseline across the seam.
        let top = body.y + theme.spacing.sidebar_pad_y();
        let width = column.w;
        // Below the guide's threshold the pane collapses to an icon rail.
        // With icons not yet drawn, labels are simply omitted rather than
        // painted clipped -- a half-cut word is worse than none.
        let compact = theme.is_compact(self.size.width);
        canvas.fill_rect_glass(column, theme.sidebar_bg);
        // The divider carries the eye down the seam between the two panes,
        // the full height of the window.
        canvas.fill_rect(
            Rect::new(
                column.right() - theme.spacing.hairline(),
                column.y,
                theme.spacing.hairline(),
                column.h,
            ),
            theme.sidebar_border,
        );
        self.paint_storage(canvas);

        let theme = self.theme;
        let active = self.sidebar.index_of(self.browser.path());
        let row_h = theme.sidebar_row();
        // Collected first because drawing needs `&mut self.type_scale` while
        // the rows live behind `&self.sidebar`.
        let rows: Vec<SidebarRow> = self.sidebar.rows().to_vec();
        for (i, row) in rows.iter().enumerate() {
            let y = top + i32::try_from(i).unwrap_or(0) * row_h;
            // Stop at the storage footer rather than painting through it.
            if y + row_h > body.y + body.h {
                break;
            }
            match row {
                // A rail has no room for headings, and the guide hides them
                // there rather than compressing them.
                SidebarRow::Heading(_) | SidebarRow::Hint(_) if compact => {}
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
                        Self::sidebar_text_x(&theme),
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
                        Self::sidebar_text_x(&theme),
                        ty,
                        theme.muted,
                        theme.sidebar_bg,
                    );
                }
                SidebarRow::Place { label, .. } => {
                    let is_active = active == Some(i);
                    let hovered = self.hover.is(Pane::Sidebar, i);
                    let pad = theme.spacing.sidebar_pad();
                    // Active outranks hover: the row marking where you *are*
                    // must not lose its treatment just because the pointer
                    // happens to be resting on it. Hover is a preview of a
                    // place you could go, so it says nothing on the place you
                    // already went.
                    let fill = match (is_active, hovered) {
                        (true, _) => Some(theme.sidebar_active),
                        (false, true) => Some(theme.hover),
                        (false, false) => None,
                    };
                    let bg = fill.map_or(theme.sidebar_bg, |fill| {
                        // The highlight is inset from both column edges and
                        // rounded, so it reads as a pill behind the row rather
                        // than as a stripe bleeding into the divider. Hover
                        // shares the geometry exactly, so the fill appears to
                        // change colour under the pointer rather than to also
                        // change shape.
                        let pill = Rect::new(pad, y, (width - pad * 2).max(0), row_h);
                        canvas.fill_round_rect(pill, fill, theme.spacing.radius());
                        if is_active {
                            // The guide's 2px accent rule on the leading edge,
                            // which is what makes the active place readable at
                            // a glance even when the fill is subtle. Kept
                            // inside the pill's straight middle section so it
                            // does not poke out of the rounded corners.
                            //
                            // Deliberately not drawn for hover: the rule is the
                            // marker for "you are here", and lending it to a
                            // transient pointer position would make the pane
                            // appear to have two active places.
                            let r = theme.spacing.radius();
                            canvas.fill_rect(
                                Rect::new(pad, y + r, theme.spacing.rule(), (row_h - r * 2).max(1)),
                                theme.accent,
                            );
                        }
                        fill
                    });
                    if compact {
                        continue;
                    }
                    // The open directory is drawn in the accent; the rest in
                    // ordinary text, so exactly one row draws the eye.
                    let color = if is_active { theme.accent } else { theme.text };
                    let tier = &mut self.type_scale.body;
                    let ty = y + (row_h - tier.line_height()) / 2;
                    let inset = Self::sidebar_text_x(&theme);
                    let room = width - inset - pad;
                    let shown = RowFormat::truncate(label, Self::columns_for(tier, room));
                    tier.draw_text(canvas, &shown, inset, ty, color, bg);
                }
            }
        }
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
        let Some(area) = self.frame().storage() else {
            return;
        };
        let Some(storage) = self.storage.clone() else {
            return;
        };
        canvas.fill_rect_glass(area, theme.sidebar_bg);
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

    /// Draw the visible rows across the guide's four columns.
    fn paint_rows(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let band = self.frame().rows();
        let (left, top) = (band.x, band.y);
        let content_width = self.browser.pane().content_width();
        let columns = self.list_columns();
        // One clock read for the whole frame: `modified` is relative to now,
        // and re-reading the clock per row could straddle midnight and render
        // two rows of the same directory against different "today"s.
        let now = localtime::LocalTime::now();

        // Gathered up front: drawing needs `&mut self.type_scale`, which
        // cannot coexist with an iterator borrowing `self.browser`.
        let rows: Vec<PaintedRow> = self
            .browser
            .pane()
            .visible_rows()
            .filter_map(|row| {
                let y = self.browser.pane().row_y(row)?;
                let entry = self.browser.entry(row)?;
                Some(PaintedRow {
                    y,
                    name: RowFormat::name(entry),
                    // Detection is skipped for directories and links, whose
                    // label is structural -- that avoids a database lookup
                    // for the majority of rows in a typical tree.
                    kind: KindLabel::of(entry, None),
                    modified: now.map_or_else(
                        || Moment::UNKNOWN.to_owned(),
                        |now| Moment::relative(entry.modified, now),
                    ),
                    size: RowFormat::size(entry),
                    color: RowFormat::color(entry, &theme),
                    selected: self.browser.pane().is_selected(row),
                    is_cursor: self.browser.pane().cursor() == Some(row),
                    hovered: self.hover.is(Pane::List, row),
                    drop_target: self.drop_hover_row == Some(row),
                    // The highlight colour follows what the row *is*, not what
                    // its name looks like, so a directory keeps its tint even
                    // when the name column is truncated to nothing.
                    row_kind: RowKind::of(entry),
                })
            })
            .collect();

        for row in rows {
            let y = top + row.y;
            let rect = Rect::new(left, y, content_width, theme.row_height);
            let radius = theme.spacing.radius();
            // Selection outranks hover on the same row: it is a state the user
            // chose, so it must not be overwritten by the pointer merely
            // passing over it. Both are washes of the row's own type colour --
            // see `Theme::hover_dir` -- so the fill says "directory" or "file"
            // even while it covers the coloured name that would have said so.
            // The keyboard cursor is folded in as a *fill* rather than the
            // outline it used to be: an outline is a border, and a border on
            // the highlight is exactly what was asked to go. It still has to
            // be locatable, so an unselected cursor row takes the hover wash --
            // the same treatment the pointer gives, since both mean "this row
            // is where your attention is". Only when focused: claiming
            // keyboard focus in an unfocused window is a lie.
            let cursor_here = self.focused && row.is_cursor;
            let bg = match (row.selected, row.hovered || cursor_here) {
                (true, _) => {
                    let fill = row.row_kind.selection_fill(&theme);
                    canvas.fill_round_rect(rect, fill, radius);
                    fill
                }
                (false, true) => {
                    let fill = row.row_kind.hover_fill(&theme);
                    canvas.fill_round_rect(rect, fill, radius);
                    fill
                }
                (false, false) => theme.background,
            };
            // A drop-target outline is drawn over whatever fill the row
            // already has -- selection and hover are states the row already
            // carries; "a drag is hovering here" is a distinct, temporary
            // fact about the pointer, not a replacement for them.
            if row.drop_target {
                canvas.draw_round_rect(rect, row.row_kind.selection_fill(&theme), radius);
            }
            // A thin separator per row rather than a card, per the guide.
            canvas.fill_rect(
                Rect::new(
                    left,
                    y + theme.row_height - theme.spacing.hairline(),
                    content_width,
                    theme.spacing.hairline(),
                ),
                theme.sidebar_border,
            );

            for cell in columns.cells() {
                // The filename keeps the body tier and the type colour; the
                // metadata is subordinate, so it takes the smaller tier and
                // the muted colour -- until its row is selected, where the
                // guide asks for the whole row to brighten.
                let (text, color, tier) = match cell.column {
                    Column::Name => (&row.name, row.color, &mut self.type_scale.body),
                    Column::Modified => (
                        &row.modified,
                        Self::meta_color(&theme, row.selected),
                        &mut self.type_scale.detail,
                    ),
                    Column::Kind => (
                        &row.kind,
                        Self::meta_color(&theme, row.selected),
                        &mut self.type_scale.detail,
                    ),
                    Column::Size => (
                        &row.size,
                        Self::meta_color(&theme, row.selected),
                        &mut self.type_scale.detail,
                    ),
                };
                let text_y = y + (theme.row_height - tier.line_height()) / 2;
                let shown = RowFormat::truncate(text, Self::columns_for(tier, cell.width));
                let text_x = cell.text_x(tier.measure(&shown));
                tier.draw_text(canvas, &shown, text_x, text_y, color, bg);
            }
        }
    }

    /// The colour for a metadata cell.
    ///
    /// The guide's rule: metadata stays subdued *until its row is selected*,
    /// at which point holding it muted against the selection fill would make
    /// it the least readable thing on screen.
    const fn meta_color(theme: &Theme, selected: bool) -> Color {
        if selected { theme.text } else { theme.muted }
    }

    /// Draw square cards for the grid presentation.
    fn paint_grid(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let band = self.frame().rows();
        let cells = self.browser.grid().visible_cells();
        for cell in cells {
            let Some(entry) = self.browser.entry(cell.index) else {
                continue;
            };
            let rect = Rect::new(band.x + cell.x, band.y + cell.y, cell.width, cell.height);
            let selected = self.browser.grid().is_selected(cell.index);
            let hovered = self.hover.is(Pane::List, cell.index);
            let cursor = self.focused && self.browser.grid().cursor() == Some(cell.index);
            let kind = RowKind::of(entry);
            let background = if selected {
                let fill = kind.selection_fill(&theme);
                canvas.fill_round_rect(rect, fill, theme.spacing.radius());
                fill
            } else {
                if hovered || cursor {
                    // Grid hover is deliberately an outline, preserving the
                    // guide's quiet card geometry instead of turning cards
                    // into list-row surfaces.
                    canvas.draw_round_rect(rect, kind.hover_fill(&theme), theme.spacing.radius());
                }
                theme.background
            };
            let icon = if entry.is_navigable() { "□" } else { "◇" };
            let icon_tier = &mut self.type_scale.title;
            let icon_w = icon_tier.measure(icon);
            icon_tier.draw_text(
                canvas,
                icon,
                rect.x + (rect.w - icon_w) / 2,
                rect.y + theme.spacing.m(),
                RowFormat::color(entry, &theme),
                background,
            );
            let label = RowFormat::truncate(
                &RowFormat::name(entry),
                Self::columns_for(&mut self.type_scale.detail, rect.w - theme.spacing.m() * 2),
            );
            let detail = &mut self.type_scale.detail;
            let label_w = detail.measure(&label);
            detail.draw_text(
                canvas,
                &label,
                rect.x + (rect.w - label_w) / 2,
                rect.bottom() - theme.spacing.m() - detail.line_height(),
                theme.text,
                background,
            );
        }
    }

    /// Draw the active presentation's scrollbar, if it overflows.
    fn paint_scrollbar(&self, canvas: &mut Canvas<'_>) {
        let (visible, gutter_x, scrollbar, viewport) = match self.browser.view_mode() {
            ViewMode::List => {
                let pane = self.browser.pane();
                (
                    pane.scrollbar_visible(),
                    pane.gutter_x(),
                    pane.scrollbar(),
                    pane.viewport(),
                )
            }
            ViewMode::Grid => {
                let grid = self.browser.grid();
                (
                    grid.scrollbar_visible(),
                    grid.gutter_x(),
                    grid.scrollbar(),
                    grid.viewport(),
                )
            }
        };
        if !visible {
            return;
        }
        let theme = &self.theme;
        let band = self.frame().rows();
        let x = band.x + gutter_x;
        let gutter = (band.right() - x).max(0);
        canvas.fill_rect(Rect::new(x, band.y, gutter, band.h), theme.scrollbar_track);
        let thumb = scrollbar.thumb(viewport);
        // The thumb is rounded to its own half-width, i.e. a capsule: it is a
        // free-floating element with nothing to align a square edge against.
        let thumb_w = (gutter - 4).max(1);
        canvas.fill_round_rect(
            Rect::new(x + 2, band.y + thumb.offset, thumb_w, thumb.length),
            theme.scrollbar_thumb,
            thumb_w / 2,
        );
    }

    /// Draw the status line: the filter box, or a summary.
    fn paint_status(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let band = self.frame().status();
        let y = band.y;
        let filtering = self.browser.is_filtering();
        let background = if filtering {
            theme.filter_bg
        } else {
            theme.status_bg
        };
        canvas.fill_rect_glass(band, background);
        // A hairline above the strip, mirroring the one under the toolbar, so
        // the right column reads as distinct bands.
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
        let shown = self.browser.len();
        let total = self.browser.total();
        let summary = if shown == total {
            format!("{total} entries")
        } else {
            format!("{shown} of {total} entries")
        };
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

        self.paint_status_hint(canvas, y, background);
    }

    /// Draw the keyboard hint at the status line's right end.
    ///
    /// The guide gives the hint a minimal keycap: an outlined box around the
    /// chord with its description beside it in muted text.
    fn paint_status_hint(&mut self, canvas: &mut Canvas<'_>, y: i32, background: Color) {
        let theme = self.theme;
        // Which hint earns the space depends on what the user can do next:
        // while filtering, escaping is what they will want.
        let (description, chord) = if self.browser.is_filtering() {
            ("clear filter", "esc")
        } else {
            ("select all", "ctrl+a")
        };

        let band = self.frame().status();
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
        // The right column is filled explicitly rather than relying on the
        // clear: the sidebar and the list may take different backgrounds, and
        // painting the column that owns the list makes the seam between them
        // independent of whatever the window's clear colour happens to be.
        //
        // `fill_rect_glass`, not `fill_rect`: this is background, so on a
        // translucent window it must stay as see-through as the clear it
        // covers. Painting it solid would make the whole list area opaque and
        // leave the window transparent only in the slivers no band reaches.
        canvas.fill_rect_glass(self.frame().main(), self.theme.background);
        self.paint_toolbar(canvas);
        self.paint_sidebar(canvas);
        if self.browser.view_mode() == ViewMode::List {
            self.paint_column_headers(canvas);
            self.paint_rows(canvas);
        } else {
            self.paint_grid(canvas);
        }
        self.paint_scrollbar(canvas);
        self.paint_status(canvas);
    }

    fn on_key(&mut self, press: KeyPress) {
        let outcome = self.browser.key(press);
        self.apply(outcome);
    }

    fn on_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn on_pointer(&mut self, event: PointerEventKind) {
        match event {
            PointerEventKind::Press {
                button: PointerButton::Left,
                at,
                modifiers,
            } => {
                // The toolbar is checked before either pane: its controls sit
                // above the list, and a click on one must not fall through to
                // whatever row happens to be under it.
                if let Some(target) = self.toolbar_hit(at) {
                    self.activate_control(target);
                    return;
                }
                // A click in the places pane navigates and goes no further:
                // it must not also be routed to the list beside it.
                if self.in_sidebar(at) {
                    if let Some(path) = self
                        .sidebar_row_at(at)
                        .and_then(|i| self.sidebar.rows().get(i))
                        .and_then(SidebarRow::path)
                        .map(std::path::Path::to_path_buf)
                    {
                        let outcome = self.browser.go_to(&path);
                        self.apply(outcome);
                        // A new directory may live on a different filesystem.
                        self.refresh_storage();
                    }
                    return;
                }
                let Some((x, y)) = self.to_pane(at) else {
                    return;
                };
                let outcome = if self.is_double_click(Self::now_ms(), at) {
                    self.browser.double_click(x, y)
                } else {
                    self.browser.press(
                        x,
                        y,
                        Mods {
                            ctrl: modifiers.ctrl(),
                            shift: modifiers.shift(),
                        },
                    )
                };
                self.apply(outcome);
                // Arm a potential drag from this press -- but only over a
                // real row. A press on empty list space or a scrollbar drag
                // must never turn into a file drag.
                self.drag_row = self.browser.row_at(x, y);
                if self.drag_row.is_some() {
                    self.drag_arm.press((at.x, at.y));
                }
            }
            PointerEventKind::Release { .. } => {
                let outcome = self.browser.release();
                self.apply(outcome);
                self.drag_arm.cancel();
                self.drag_row = None;
            }
            PointerEventKind::Motion(at) => {
                // A drag is armed only while a row press hasn't yet crossed
                // the threshold; once it fires this is a one-shot -- the
                // compositor takes over pointer delivery for the rest of the
                // gesture the moment `start_drag` is queued.
                if let Some(row) = self.drag_row
                    && self.drag_arm.motion_exceeds_threshold((at.x, at.y))
                {
                    self.pending_drag = Some(self.browser.drag_paths(row));
                    self.drag_row = None;
                    return;
                }
                // Motion is forwarded unconditionally rather than only when
                // inside the list: a scrollbar drag that wanders over the
                // header must keep scrolling, exactly as it does everywhere
                // else. The pane ignores it unless a drag is live.
                let rows_top = self.frame().rows().y;
                let outcome = self.browser.motion(at.y - rows_top);
                self.apply(outcome);
                // Hover is tracked separately from the drag above, and only
                // over the list proper: `to_pane` rejects the gutter and the
                // chrome, so sliding along the scrollbar or across the toolbar
                // correctly drops the row highlight instead of leaving the last
                // row lit.
                //
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
            PointerEventKind::Scroll { lines, .. } => {
                let outcome = self.browser.scroll(lines);
                self.apply(outcome);
            }
            // The pointer left the surface, so the highlight must go with it;
            // otherwise a row stays lit while the pointer is in another window.
            PointerEventKind::Leave => {
                let _ = self.hover.clear();
                self.drag_arm.cancel();
                self.drag_row = None;
            }
            // Enter carries a position, but the Motion event that follows it
            // does the hover work -- handling it here too would only duplicate
            // that. A non-left press stays deliberately inert until there is a
            // context menu to open.
            PointerEventKind::Enter(_) | PointerEventKind::Press { .. } => {}
        }
    }

    fn on_files_dropped(&mut self, paths: Vec<std::path::PathBuf>, at: (i32, i32)) {
        let rows = self.frame().rows();
        let target = self.browser.drop_target_dir(at.0 - rows.x, at.1 - rows.y);
        let outcome = self.browser.accept_drop(&paths, &target);
        self.apply(outcome);
        self.drop_hover_row = None;
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
        let user_dirs = crate::user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        // The temp dir always exists and is readable, so `open` succeeds
        // without depending on anything in the developer's home.
        let browser = crate::browser::Browser::open(&std::env::temp_dir(), 700, 400, 28)
            .expect("the temp dir must be listable");
        let mut painter = FilesWindow::new(browser, theme, type_scale, sidebar, favorites, size);
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
        let user_dirs = crate::user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        let browser = crate::browser::Browser::open(&std::env::temp_dir(), 700, 400, 28)
            .expect("the temp dir must be listable");
        let mut painter = FilesWindow::new(browser, theme, type_scale, sidebar, favorites, size);
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

        let cfg = config::Config::default();
        let theme = crate::theme::Theme::from_config(&cfg);
        let type_scale = crate::typography::Typography::load(&cfg, 1.0);
        let favorites = crate::favorites::Favorites::default();
        let user_dirs = crate::user_dirs::UserDirs::load();
        let sidebar = crate::sidebar::Sidebar::new(&user_dirs, &favorites);
        let browser = crate::browser::Browser::open(&root, 700, 400, 28).expect("listable");
        let painter = FilesWindow::new(browser, theme, type_scale, sidebar, favorites, size);
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
        let bar = painter.toolbar();
        let rect = match target {
            Target::Back => bar.back(),
            Target::Forward => bar.forward(),
            Target::Filter => bar.filter(),
            Target::ViewList => bar.view_segment(0),
            Target::ViewGrid => bar.view_segment(1),
        }
        .expect("the control must be drawn at this window size");
        super::Position {
            x: rect.x + rect.w / 2,
            y: rect.y + rect.h / 2,
        }
    }

    /// Click a toolbar control by its target, through the real pointer path.
    fn click_control(painter: &mut FilesWindow, target: Target) {
        let at = control_centre(painter, target);
        painter.on_pointer(PointerEventKind::Press {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
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

    /// A click on the filter box must focus it, so typing goes into the box
    /// rather than being treated as list shortcuts.
    #[test]
    fn clicking_the_filter_box_focuses_it() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("toolbar-filter");
        assert!(!painter.browser.is_filtering(), "it starts unfocused");
        click_control(&mut painter, Target::Filter);
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
        click_control(&mut painter, Target::Filter);
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
        let at = control_centre(&painter, Target::Filter);
        painter.on_pointer(PointerEventKind::Motion(at));
        assert_eq!(
            painter.hover.control(),
            Some(Target::Filter),
            "the pointer is over the filter box"
        );
        assert!(!painter.hover.is(Pane::List, 0), "and so no row may be lit");
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
            painter.hover.move_to(Some(Spot::Row(Pane::List, 0))),
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

        assert!(painter.hover.move_to(Some(Spot::Row(Pane::Sidebar, place))));
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
        let row_h = painter.theme.sidebar_row();
        let top = painter.frame().sidebar_body().y + painter.theme.spacing.sidebar_pad_y();
        let at = super::Position {
            x: 20,
            y: top + i32::try_from(heading).unwrap_or(0) * row_h + row_h / 2,
        };
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
        assert!(painter.hover.move_to(Some(Spot::Row(Pane::Sidebar, index))));
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

        painter.hover.move_to(Some(Spot::Row(Pane::List, 0)));
        let dir_hovered = paint_opaque(&mut painter, size);
        painter.hover.move_to(Some(Spot::Row(Pane::List, 1)));
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

    /// The pane-relative position of the centre of list row `row`, as a
    /// window-relative pointer position -- what a real click on that row
    /// would report.
    fn row_centre(painter: &FilesWindow, row: usize) -> super::Position {
        let rows = painter.frame().rows();
        let y = painter
            .browser
            .pane()
            .row_y(row)
            .expect("row must be visible");
        super::Position {
            x: rows.x + 10,
            y: rows.y + y + painter.theme.row_height / 2,
        }
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
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("drag-plain-click");
        let at = row_centre(&painter, 0);
        left_press(&mut painter, at);
        painter.on_pointer(PointerEventKind::Release {
            button: PointerButton::Left,
            at,
            modifiers: platform::toplevel::ModifierState::default(),
        });
        assert!(painter.take_pending_drag().is_none());
    }

    /// Moving the pointer past the drag threshold after pressing on a row
    /// must queue that row's path as a pending drag.
    #[test]
    fn dragging_past_the_threshold_queues_the_row_for_a_drag() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("drag-threshold");
        let at = row_centre(&painter, 0);
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

    /// A press on empty space below the list (no row under the pointer) must
    /// never arm a drag, no matter how far the pointer then moves.
    #[test]
    fn pressing_empty_space_never_arms_a_drag() {
        let (mut painter, _root, size) = painter_over_a_mixed_directory("drag-empty-space");
        let rows = painter.frame().rows();
        let at = super::Position {
            x: rows.x + 10,
            y: rows.y + rows.h - 2,
        };
        left_press(&mut painter, at);
        let moved = super::Position {
            x: at.x + 50,
            y: at.y,
        };
        painter.on_pointer(PointerEventKind::Motion(moved));
        assert!(painter.take_pending_drag().is_none());
        let _ = size;
    }

    /// Dropping files onto a directory row must move them there and refresh
    /// the listing -- the full path from `Painter::on_files_dropped` through
    /// to a real filesystem mutation.
    #[test]
    fn dropping_files_onto_a_directory_row_moves_them_there() {
        let (mut painter, root, _size) = painter_over_a_mixed_directory("drag-drop-onto-dir");
        let file = root.join("b_file.txt");
        let folder = root.join("a_directory");
        assert!(
            painter
                .browser
                .entry(0)
                .is_some_and(fsmodel::entry::Entry::is_navigable),
            "row 0 must be the directory to drop onto"
        );
        let at = row_centre(&painter, 0);
        painter.on_files_dropped(vec![file.clone()], (at.x, at.y));
        assert!(!file.exists(), "the file must have moved out of the root");
        assert!(
            folder.join("b_file.txt").exists(),
            "the file must have landed inside the dropped-on directory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Reporting drag-hover state must resolve to the row under the given
    /// position, so the drop-target outline lands on the row a drop would
    /// actually use.
    #[test]
    fn update_drag_hover_resolves_to_the_row_under_the_position() {
        let (mut painter, _root, _size) = painter_over_a_mixed_directory("drag-hover-resolve");
        let at = row_centre(&painter, 0);
        painter.update_drag_hover(true, (at.x, at.y));
        assert_eq!(painter.drop_hover_row, Some(0));
        painter.update_drag_hover(false, (at.x, at.y));
        assert_eq!(
            painter.drop_hover_row, None,
            "the highlight must clear once the drag leaves"
        );
    }
}
