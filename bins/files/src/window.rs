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
use crate::kind_label::KindLabel;
use crate::moment::Moment;
use crate::row_format::RowFormat;
use crate::sidebar::{Sidebar, SidebarRow};
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
    /// Set when the user asks to quit, read by the event loop.
    quit: bool,
    /// A file the user activated, for the loop to hand to the system opener.
    ///
    /// Queued rather than spawned here: launching a process from inside a
    /// paint callback would run it while the window is mid-frame.
    pending_open: Option<std::path::PathBuf>,
    /// When and where the last left-click landed, for double-click detection.
    last_click: Option<(u64, Position)>,
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
            quit: false,
            pending_open: None,
            last_click: None,
        }
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
        let x = self.theme.list_x(self.sidebar.visible(), self.size.width);
        self.browser.resize(
            (self.size.width - x).max(0),
            self.theme.list_height(self.size.height),
        );
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
            Outcome::Ignored | Outcome::Redraw => {}
        }
    }

    /// Convert a pointer position into list-pane coordinates.
    ///
    /// Returns `None` above or below the list, so a click on the header or
    /// status bar cannot be mistaken for a click on whichever row happens to
    /// line up with it -- and `None` over the sidebar, so a click on a place
    /// does not also land on the row beside it.
    fn to_pane(&self, at: Position) -> Option<(i32, i32)> {
        let y = at.y - self.theme.list_top();
        if y < 0 || y >= self.theme.list_height(self.size.height) {
            return None;
        }
        let x = at.x - self.theme.list_x(self.sidebar.visible(), self.size.width);
        if x < 0 {
            return None;
        }
        Some((x, y))
    }

    /// Whether `at` falls inside the places pane.
    ///
    /// The pane begins at the toolbar rather than below the column headers,
    /// matching where it is painted -- a mismatch here would leave a dead
    /// strip at the top of the sidebar where clicks did nothing.
    fn in_sidebar(&self, at: Position) -> bool {
        self.sidebar.visible()
            && at.x < self.theme.sidebar_width_at(self.size.width)
            && at.y >= self.theme.header_height
            && at.y < self.size.height - self.theme.status_height
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

    /// Draw the toolbar: the breadcrumb path and the filter control.
    ///
    /// The guide's toolbar also carries back/forward, a view switch and a
    /// new-directory action. Those are deliberately not drawn yet: a control
    /// that is visible but inert is worse than an absent one, because the
    /// user learns it is broken. They arrive with the actions behind them.
    fn paint_toolbar(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        canvas.fill_rect(
            Rect::new(0, 0, self.size.width, theme.header_height),
            theme.toolbar_bg,
        );
        // A hairline under the toolbar separates it from the list without
        // spending a whole row of vertical space on a gap.
        canvas.fill_rect(
            Rect::new(
                0,
                theme.header_height - theme.spacing.hairline(),
                self.size.width,
                theme.spacing.hairline(),
            ),
            theme.sidebar_border,
        );
        self.paint_breadcrumb(canvas);
        self.paint_filter_control(canvas);
    }

    /// Draw the breadcrumb path, most-significant segment first.
    ///
    /// The current segment is drawn in full-strength text and the ancestors
    /// muted, so the eye lands on where you *are* rather than on the path you
    /// took to get there.
    fn paint_breadcrumb(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let crumbs = self.crumbs();
        // The filter control owns the right end of the toolbar, so the path
        // gets what is left after it and the gaps around it.
        let reserved = theme.spacing.search_width() + theme.spacing.l() * 2;
        let limit = self.size.width - reserved;
        let mut x = theme.spacing.l();

        let (shown, elided) = crumbs.tail(self.crumb_budget(limit));
        if elided {
            let label = &mut self.type_scale.label;
            let y = label.baseline_y(theme.header_height);
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
                let y = label.baseline_y(theme.header_height);
                x = label.draw_text(canvas, "/", x, y, theme.muted, theme.toolbar_bg);
                x += theme.spacing.xs();
            }
            let (color, tier) = if crumb.current {
                (theme.text, &mut self.type_scale.title)
            } else {
                (theme.muted, &mut self.type_scale.body)
            };
            let y = tier.baseline_y(theme.header_height);
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
        let height = theme.spacing.control_height();
        let x = self.size.width - width - theme.spacing.l();
        let y = (theme.header_height - height) / 2;
        // Guard against a window too narrow to hold the control at all --
        // drawing it at a negative x would wrap around the canvas.
        if x < theme.spacing.l() {
            return;
        }
        let rect = Rect::new(x, y, width, height);
        canvas.fill_rect(rect, theme.filter_bg);
        canvas.draw_rect(
            rect,
            if filtering {
                theme.focus_ring
            } else {
                theme.control_border
            },
        );

        let inner_x = x + theme.spacing.s();
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
        let y = theme.column_header_top();
        let height = theme.spacing.column_header();
        let left = theme.list_x(self.sidebar.visible(), self.size.width);
        let width = (self.size.width - left).max(0);
        canvas.fill_rect(Rect::new(left, y, width, height), theme.background);

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
        let left = theme.list_x(self.sidebar.visible(), self.size.width);
        // The rows reserve the scrollbar gutter, so the headers must too or
        // the "size" heading floats away from the numbers below it.
        let content = self.browser.pane().content_width();
        Columns::new(
            left + theme.padding,
            (content - theme.padding * 2).max(0),
            theme.spacing.s(),
            theme.is_compact(self.size.width),
        )
    }

    /// Draw the places sidebar: section headings, places, and bookmarks.
    fn paint_sidebar(&mut self, canvas: &mut Canvas<'_>) {
        if !self.sidebar.visible() {
            return;
        }
        let theme = self.theme;
        // The pane starts at the toolbar, not below the column headers: the
        // headings belong to the list, and a sidebar that skipped that band
        // would leave an unpainted notch at its top.
        let top = theme.header_height;
        let height = theme.pane_height(self.size.height);
        let width = theme.sidebar_width_at(self.size.width);
        // Below the guide's threshold the pane collapses to an icon rail.
        // With icons not yet drawn, labels are simply omitted rather than
        // painted clipped -- a half-cut word is worse than none.
        let compact = theme.is_compact(self.size.width);
        canvas.fill_rect(Rect::new(0, top, width, height), theme.sidebar_bg);
        // The divider carries the eye down the seam between the two panes.
        canvas.fill_rect(
            Rect::new(
                width - theme.spacing.hairline(),
                top,
                theme.spacing.hairline(),
                height,
            ),
            theme.sidebar_border,
        );

        let active = self.sidebar.index_of(self.browser.path());
        let row_h = theme.spacing.sidebar_row();
        // Collected first because drawing needs `&mut self.type_scale` while
        // the rows live behind `&self.sidebar`.
        let rows: Vec<SidebarRow> = self.sidebar.rows().to_vec();
        for (i, row) in rows.iter().enumerate() {
            let y = top + i32::try_from(i).unwrap_or(0) * row_h;
            // Stop at the bottom of the pane rather than painting under the
            // status strip.
            if y + row_h > top + height {
                break;
            }
            match row {
                // A rail has no room for headings, and the guide hides them
                // there rather than compressing them.
                SidebarRow::Heading(_) | SidebarRow::Hint(_) if compact => {}
                SidebarRow::Heading(text) => {
                    // Headings sit in the small label tier and the muted
                    // colour, so they read as structure rather than as
                    // another place you could click.
                    let label = &mut self.type_scale.label;
                    let ty = y + (row_h - label.line_height()) / 2;
                    label.draw_text(
                        canvas,
                        text,
                        theme.spacing.m(),
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
                        theme.spacing.m(),
                        ty,
                        theme.muted,
                        theme.sidebar_bg,
                    );
                }
                SidebarRow::Place { label, .. } => {
                    let is_active = active == Some(i);
                    let bg = if is_active {
                        canvas.fill_rect(
                            Rect::new(0, y, width - theme.spacing.hairline(), row_h),
                            theme.sidebar_active,
                        );
                        // The guide's 2px accent rule on the left edge, which
                        // is what makes the active place readable at a glance
                        // even when the fill is subtle.
                        canvas
                            .fill_rect(Rect::new(0, y, theme.spacing.rule(), row_h), theme.accent);
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
                    let body = &mut self.type_scale.body;
                    let ty = y + (row_h - body.line_height()) / 2;
                    let inset = theme.spacing.m();
                    let room = width - inset - theme.spacing.s();
                    let shown = RowFormat::truncate(label, Self::columns_for(body, room));
                    body.draw_text(canvas, &shown, inset, ty, color, bg);
                }
            }
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
        let left = theme.list_x(self.sidebar.visible(), self.size.width);
        let top = theme.list_top();
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
                })
            })
            .collect();

        for row in rows {
            let y = top + row.y;
            let rect = Rect::new(left, y, content_width, theme.row_height);
            let bg = if row.selected {
                canvas.fill_rect(rect, theme.selection);
                theme.selection
            } else {
                theme.background
            };
            // The cursor outline is drawn even on an unselected row, and only
            // when focused: an outline on an unfocused window claims a
            // keyboard focus we do not have.
            if self.focused && row.is_cursor {
                canvas.draw_rect(rect, theme.cursor);
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

    /// Draw the scrollbar, if the list overflows.
    fn paint_scrollbar(&self, canvas: &mut Canvas<'_>) {
        let pane = self.browser.pane();
        if !pane.scrollbar_visible() {
            return;
        }
        let theme = &self.theme;
        let x = theme.list_x(self.sidebar.visible(), self.size.width) + pane.gutter_x();
        let track_h = theme.list_height(self.size.height);
        let gutter = (self.size.width - x).max(0);
        canvas.fill_rect(
            Rect::new(x, theme.list_top(), gutter, track_h),
            theme.scrollbar_track,
        );
        let thumb = pane.scrollbar().thumb(pane.viewport());
        canvas.fill_rect(
            Rect::new(
                x + 2,
                theme.list_top() + thumb.offset,
                (gutter - 4).max(1),
                thumb.length,
            ),
            theme.scrollbar_thumb,
        );
    }

    /// Draw the status line: the filter box, or a summary.
    fn paint_status(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let y = self.size.height - theme.status_height;
        let filtering = self.browser.is_filtering();
        let background = if filtering {
            theme.filter_bg
        } else {
            theme.status_bg
        };
        canvas.fill_rect(
            Rect::new(0, y, self.size.width, theme.status_height),
            background,
        );
        // A hairline above the strip, mirroring the one under the header, so
        // the window reads as three distinct bands.
        canvas.fill_rect(Rect::new(0, y, self.size.width, 1), theme.sidebar_border);

        // An error outranks everything else: it explains why the list is not
        // what the user expected, and hiding it behind a file count would
        // leave them staring at a stale listing wondering what happened.
        if let Some(error) = self.browser.error() {
            let message = error.message();
            let detail = &mut self.type_scale.detail;
            let text_y = y + (theme.status_height - detail.line_height()) / 2;
            detail.draw_text(
                canvas,
                &message,
                theme.padding,
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
        let text_y = y + (theme.status_height - label.line_height()) / 2;
        label.draw_text(
            canvas,
            &summary,
            theme.spacing.l(),
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

        let label = &mut self.type_scale.label;
        let pad = theme.spacing.xs();
        let chord_w = label.measure(chord) + pad * 2;
        let cap_h = label.line_height() + pad;
        let gap = theme.spacing.s();
        let total = label.measure(description) + gap + chord_w;
        let start = self.size.width - theme.spacing.l() - total;
        // A window too narrow for the hint drops it rather than overlapping
        // the entry count, which is the more useful of the two.
        let count_end = theme.spacing.l() + label.measure("9999 of 9999 entries");
        if start < count_end {
            return;
        }

        let text_y = y + (theme.status_height - label.line_height()) / 2;
        let after = label.draw_text(canvas, description, start, text_y, theme.muted, background);
        let cap_x = after + gap;
        let cap_y = y + (theme.status_height - cap_h) / 2;
        canvas.draw_rect(
            Rect::new(cap_x, cap_y, chord_w, cap_h),
            theme.control_border,
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
        canvas.clear(self.theme.background);
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
                    let y = at.y - self.theme.header_height;
                    if let Some(path) = self
                        .sidebar
                        .row_at(y, self.theme.spacing.sidebar_row())
                        .and_then(|r| r.path())
                        .map(std::path::Path::to_path_buf)
                    {
                        let outcome = self.browser.go_to(&path);
                        self.apply(outcome);
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
                let outcome = self.browser.motion(at.y - self.theme.list_top());
                self.apply(outcome);
            }
            PointerEventKind::Scroll { lines, .. } => {
                let outcome = self.browser.scroll(lines);
                self.apply(outcome);
            }
            // Enter/Leave need no action -- hover is not drawn -- and a
            // non-left press is deliberately inert until there is a context
            // menu to open.
            PointerEventKind::Enter(_)
            | PointerEventKind::Leave
            | PointerEventKind::Press { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FilesWindow;

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
