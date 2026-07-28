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
use widget::Mods;

use crate::breadcrumb::Breadcrumb;
use crate::browser::{Browser, Outcome};
use crate::columns::{Column, Columns};
use crate::favorites::Favorites;
use crate::frame::Frame;
use crate::hover::Hover;
use crate::kind_label::KindLabel;
use crate::moment::Moment;
use crate::row_format::RowFormat;
use crate::sidebar::{Sidebar, SidebarRow};
use crate::storage::Storage;
use crate::theme::Theme;
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
        }
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

    /// Draw the back and forward affordances at the toolbar's left.
    ///
    /// The guide puts chevrons here. Icons are still deferred, so these are
    /// drawn as the glyphs a monospace face definitely has -- and, crucially,
    /// they are drawn *disabled* (muted, no hover fill) because there is no
    /// history stack behind them yet. Showing them greyed is honest about
    /// what the toolbar will hold without implying a working control: the
    /// alternative, an enabled-looking button that does nothing, teaches the
    /// user the application is broken.
    fn paint_nav_buttons(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let bar = self.frame().toolbar();
        let size = theme.spacing.icon_button();
        // The guide's toolbar padding, not the generic gap step: the first
        // button must clear the window edge by the same margin the last one
        // clears the right edge.
        let mut x = bar.x + theme.spacing.toolbar_pad();
        let y = bar.y + (bar.h - size) / 2;
        for glyph in ["\u{2039}", "\u{203a}"] {
            // Never let the buttons crowd out the path itself, which is the
            // one thing in the toolbar the user actually needs.
            if x + size > bar.right() {
                return;
            }
            let tier = &mut self.type_scale.title;
            let text_w = tier.measure(glyph);
            let text_y = y + (size - tier.line_height()) / 2;
            tier.draw_text(
                canvas,
                glyph,
                x + (size - text_w) / 2,
                text_y,
                theme.disabled,
                theme.toolbar_bg,
            );
            x += size;
        }
    }

    /// Where the breadcrumb path may start: after the navigation buttons.
    fn breadcrumb_x(&self) -> i32 {
        let theme = self.theme;
        let bar = self.frame().toolbar();
        bar.x + theme.spacing.toolbar_pad() + theme.spacing.icon_button() * 2 + theme.spacing.s()
    }

    /// Draw the list/grid view toggle at the toolbar's right end.
    ///
    /// Two segments in a shared outline, the active one filled -- the guide's
    /// anatomy. Only `list` is implemented, so `grid` is drawn muted: the
    /// toggle communicates the current view (which is real information) while
    /// not offering a switch that would do nothing.
    fn paint_view_toggle(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let bar = self.frame().toolbar();
        let button = theme.spacing.view_button();
        let height = theme.control_height();
        let width = button * 2;
        let x = bar.right() - theme.spacing.toolbar_pad() - width;
        let y = bar.y + (bar.h - height) / 2;
        // Dropped entirely rather than overlapped when the toolbar is too
        // tight: the filter box and the path outrank it.
        if x <= self.breadcrumb_x() {
            return;
        }
        // The active segment is filled, which is what makes the pair read as
        // a state rather than as two buttons. Only its *outer* corners are
        // rounded -- it shares a straight edge with the segment beside it, so
        // rounding all four would leave a notch mid-pair. Drawing the fill one
        // radius wider and squaring off that overhang gives the D-shape.
        let radius = theme.spacing.radius();
        canvas.fill_round_rect(
            Rect::new(x, y, button + radius, height),
            theme.accent_surface,
            radius,
        );
        canvas.fill_rect(
            Rect::new(x + button, y, radius, height),
            theme.accent_surface,
        );
        canvas.draw_round_rect(Rect::new(x, y, width, height), theme.control_border, radius);
        for (i, (glyph, color)) in [("\u{2261}", theme.accent), ("\u{22ee}", theme.disabled)]
            .into_iter()
            .enumerate()
        {
            let bg = if i == 0 {
                theme.accent_surface
            } else {
                theme.toolbar_bg
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
        let mut x = self.breadcrumb_x();
        // The filter control and view toggle own the right end of the
        // toolbar, so the path gets what is left after them and their gaps.
        let reserved = theme.spacing.search_width()
            + theme.spacing.view_button() * 2
            + theme.spacing.toolbar_pad()
            + theme.spacing.s() * 2;
        let limit = bar.right() - x - reserved;

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
        let width = if filtering {
            theme.spacing.search_width_focused()
        } else {
            theme.spacing.search_width()
        };
        let height = theme.control_height();
        let bar = self.frame().toolbar();
        // The view toggle sits to its right, so the filter box ends before it.
        let toggle = theme.spacing.view_button() * 2 + theme.spacing.s();
        let x = bar.right() - theme.spacing.toolbar_pad() - toggle - width;
        let y = bar.y + (bar.h - height) / 2;
        // Guard against a column too narrow to hold the control at all --
        // drawing it at a negative x would wrap around the canvas.
        if x < self.breadcrumb_x() {
            return;
        }
        let rect = Rect::new(x, y, width, height);
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
                    let pad = theme.spacing.sidebar_pad();
                    let bg = if is_active {
                        // The highlight is inset from both column edges and
                        // rounded, so it reads as a pill behind the row rather
                        // than as a stripe bleeding into the divider.
                        let pill = Rect::new(pad, y, (width - pad * 2).max(0), row_h);
                        canvas.fill_round_rect(pill, theme.sidebar_active, theme.spacing.radius());
                        // The guide's 2px accent rule on the leading edge,
                        // which is what makes the active place readable at a
                        // glance even when the fill is subtle. Kept inside the
                        // pill's straight middle section so it does not poke
                        // out of the rounded corners.
                        let r = theme.spacing.radius();
                        canvas.fill_rect(
                            Rect::new(pad, y + r, theme.spacing.rule(), (row_h - r * 2).max(1)),
                            theme.accent,
                        );
                        theme.sidebar_active
                    } else {
                        theme.sidebar_bg
                    };
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
                    hovered: self.hover.is(row),
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

    /// Draw the scrollbar, if the list overflows.
    fn paint_scrollbar(&self, canvas: &mut Canvas<'_>) {
        let pane = self.browser.pane();
        if !pane.scrollbar_visible() {
            return;
        }
        let theme = &self.theme;
        let band = self.frame().rows();
        let x = band.x + pane.gutter_x();
        let gutter = (band.right() - x).max(0);
        canvas.fill_rect(Rect::new(x, band.y, gutter, band.h), theme.scrollbar_track);
        let thumb = pane.scrollbar().thumb(pane.viewport());
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
        self.paint_column_headers(canvas);
        self.paint_rows(canvas);
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
                // A click in the places pane navigates and goes no further:
                // it must not also be routed to the list beside it.
                if self.in_sidebar(at) {
                    // Rows begin at the sidebar's own inset, matching where
                    // `paint_sidebar` starts drawing them -- a mismatch here
                    // would offset every click by one row.
                    let y =
                        at.y - self.frame().sidebar_body().y - self.theme.spacing.sidebar_pad_y();
                    if y < 0 {
                        return;
                    }
                    if let Some(path) = self
                        .sidebar
                        .row_at(y, self.theme.sidebar_row())
                        .and_then(|r| r.path())
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
            }
            PointerEventKind::Release { .. } => {
                let outcome = self.browser.release();
                self.apply(outcome);
            }
            PointerEventKind::Motion(at) => {
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
                let row = self
                    .to_pane(at)
                    .and_then(|(_, y)| self.browser.pane().viewport().row_at_y(y));
                let _ = self.hover.move_to(row);
            }
            PointerEventKind::Scroll { lines, .. } => {
                let outcome = self.browser.scroll(lines);
                self.apply(outcome);
            }
            // The pointer left the surface, so the highlight must go with it;
            // otherwise a row stays lit while the pointer is in another window.
            PointerEventKind::Leave => {
                let _ = self.hover.clear();
            }
            // Enter carries a position, but the Motion event that follows it
            // does the hover work -- handling it here too would only duplicate
            // that. A non-left press stays deliberately inert until there is a
            // context menu to open.
            PointerEventKind::Enter(_) | PointerEventKind::Press { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FilesWindow, Painter};
    use platform::SurfaceFormat;
    use platform::draw::Canvas;
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

        assert!(painter.hover.move_to(Some(0)), "row 0 becomes hovered");
        let hot = paint_opaque(&mut painter, size);
        assert_ne!(cold, hot, "a hovered row must be visibly highlighted");

        // ...and clearing it must restore the original frame exactly, or the
        // highlight leaks and rows stay lit after the pointer leaves.
        assert!(painter.hover.clear(), "the hover is dropped");
        let cleared = paint_opaque(&mut painter, size);
        assert_eq!(cold, cleared, "clearing hover must restore the frame");
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

        painter.hover.move_to(Some(0));
        let dir_hovered = paint_opaque(&mut painter, size);
        painter.hover.move_to(Some(1));
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
}
