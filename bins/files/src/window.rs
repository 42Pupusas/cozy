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

use crate::browser::{Browser, Outcome};
use crate::favorites::Favorites;
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
        let x = self.theme.list_x(self.sidebar.visible());
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
    const fn to_pane(&self, at: Position) -> Option<(i32, i32)> {
        let y = at.y - self.theme.list_top();
        if y < 0 || y >= self.theme.list_height(self.size.height) {
            return None;
        }
        let x = at.x - self.theme.list_x(self.sidebar.visible());
        if x < 0 {
            return None;
        }
        Some((x, y))
    }

    /// Whether `at` falls inside the places pane.
    const fn in_sidebar(&self, at: Position) -> bool {
        self.sidebar.visible() && at.x < self.theme.sidebar_width && at.y >= self.theme.list_top()
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

    /// Draw the path header.
    fn paint_header(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        canvas.fill_rect(
            Rect::new(0, 0, self.size.width, theme.header_height),
            theme.header_bg,
        );
        // A hairline under the header separates it from the list without
        // spending a whole row of vertical space on a gap.
        canvas.fill_rect(
            Rect::new(0, theme.header_height - 1, self.size.width, 1),
            theme.sidebar_border,
        );
        let title = &mut self.type_scale.title;
        let text_y = title.baseline_y(theme.header_height);
        let width = self.size.width - theme.padding * 2;
        let path = self.browser.path().display().to_string();
        // Truncated from the *left*: the tail of a path (where you are) is
        // far more useful than the head (that you are under /home).
        let columns = Self::columns_for(title, width);
        let shown = Self::truncate_left(&path, columns);
        title.draw_text(
            canvas,
            &shown,
            theme.padding,
            text_y,
            theme.text,
            theme.header_bg,
        );
    }

    /// Draw the places sidebar: section headings, places, and bookmarks.
    fn paint_sidebar(&mut self, canvas: &mut Canvas<'_>) {
        if !self.sidebar.visible() {
            return;
        }
        let theme = self.theme;
        let top = theme.list_top();
        let height = theme.list_height(self.size.height);
        canvas.fill_rect(
            Rect::new(0, top, theme.sidebar_width, height),
            theme.sidebar_bg,
        );
        // The divider carries the eye down the seam between the two panes.
        canvas.fill_rect(
            Rect::new(theme.sidebar_width - 1, top, 1, height),
            theme.sidebar_border,
        );

        let active = self.sidebar.index_of(self.browser.path());
        let row_h = theme.row_height;
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
                SidebarRow::Heading(text) => {
                    // Headings sit in the small label tier and the muted
                    // colour, so they read as structure rather than as
                    // another place you could click.
                    let label = &mut self.type_scale.label;
                    let ty = y + (row_h - label.line_height()) / 2;
                    label.draw_text(
                        canvas,
                        text,
                        theme.padding,
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
                        theme.padding * 2,
                        ty,
                        theme.muted,
                        theme.sidebar_bg,
                    );
                }
                SidebarRow::Place { label, .. } => {
                    let is_active = active == Some(i);
                    let bg = if is_active {
                        canvas.fill_rect(
                            Rect::new(0, y, theme.sidebar_width - 1, row_h),
                            theme.sidebar_active,
                        );
                        theme.sidebar_active
                    } else {
                        theme.sidebar_bg
                    };
                    // The open directory is drawn in full-strength text; the
                    // rest in the directory colour, matching the list.
                    let color = if is_active { theme.text } else { theme.dir };
                    let body = &mut self.type_scale.body;
                    let ty = y + (row_h - body.line_height()) / 2;
                    let room = theme.sidebar_width - theme.padding * 3;
                    let shown = RowFormat::truncate(label, Self::columns_for(body, room));
                    body.draw_text(canvas, &shown, theme.padding * 2, ty, color, bg);
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

    /// Keep the last `columns` characters of a path, marking the cut.
    fn truncate_left(path: &str, columns: usize) -> String {
        let len = path.chars().count();
        if len <= columns {
            return path.to_owned();
        }
        if columns <= 1 {
            return "…".repeat(columns);
        }
        let skip = len - (columns - 1);
        let mut out = String::from("…");
        out.extend(path.chars().skip(skip));
        out
    }

    /// Draw the visible rows.
    fn paint_rows(&mut self, canvas: &mut Canvas<'_>) {
        let theme = self.theme;
        let left = theme.list_x(self.sidebar.visible());
        let top = theme.list_top();
        let content_width = self.browser.pane().content_width();
        // The size column is right-aligned in a fixed gutter, so names in a
        // list of mixed sizes still line up.
        let size_width = self.type_scale.detail.measure("999.9 MB");
        let name_width = (content_width - size_width - theme.padding * 3).max(0);
        let name_columns = Self::columns_for(&mut self.type_scale.body, name_width);

        // Gathered up front: drawing needs `&mut self.type_scale`, which
        // cannot coexist with an iterator borrowing `self.browser`.
        let rows: Vec<(i32, String, String, Color, bool, bool)> = self
            .browser
            .pane()
            .visible_rows()
            .filter_map(|row| {
                let y = self.browser.pane().row_y(row)?;
                let entry = self.browser.entry(row)?;
                Some((
                    y,
                    RowFormat::name(entry),
                    RowFormat::size(entry),
                    RowFormat::color(entry, &theme),
                    self.browser.pane().is_selected(row),
                    self.browser.pane().cursor() == Some(row),
                ))
            })
            .collect();

        for (y, name, size, color, selected, is_cursor) in rows {
            let y = top + y;
            let rect = Rect::new(left, y, content_width, theme.row_height);
            let bg = if selected {
                canvas.fill_rect(rect, theme.selection);
                theme.selection
            } else {
                theme.background
            };
            // The cursor outline is drawn even on an unselected row, and only
            // when focused: an outline on an unfocused window claims a
            // keyboard focus we do not have.
            if self.focused && is_cursor {
                canvas.draw_rect(rect, theme.cursor);
            }

            let body = &mut self.type_scale.body;
            let text_y = y + (theme.row_height - body.line_height()) / 2;
            let shown = RowFormat::truncate(&name, name_columns);
            body.draw_text(canvas, &shown, left + theme.padding, text_y, color, bg);

            // Right-aligned by measuring the string: with a proportional face
            // a character count is no longer a pixel width.
            let detail = &mut self.type_scale.detail;
            let size_x = left + content_width - theme.padding - detail.measure(&size);
            let detail_y = y + (theme.row_height - detail.line_height()) / 2;
            detail.draw_text(canvas, &size, size_x, detail_y, theme.muted, bg);
        }
    }

    /// Draw the scrollbar, if the list overflows.
    fn paint_scrollbar(&self, canvas: &mut Canvas<'_>) {
        let pane = self.browser.pane();
        if !pane.scrollbar_visible() {
            return;
        }
        let theme = &self.theme;
        let x = theme.list_x(self.sidebar.visible()) + pane.gutter_x();
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

        if filtering {
            let query = self.browser.filter().text().to_owned();
            let before = self.browser.filter().before_caret().to_owned();
            let detail = &mut self.type_scale.detail;
            let text_y = y + (theme.status_height - detail.line_height()) / 2;
            let prefix = "filter: ";
            let query_x = detail.draw_text(
                canvas,
                prefix,
                theme.padding,
                text_y,
                theme.muted,
                background,
            );
            detail.draw_text(canvas, &query, query_x, text_y, theme.text, background);
            // Measured, not counted: with a proportional face the caret's
            // pixel offset is the width of the text before it.
            let caret_x = query_x + detail.measure(&before);
            canvas.fill_rect(
                Rect::new(caret_x, text_y, 2, detail.line_height()),
                theme.caret,
            );
            return;
        }

        let shown = self.browser.len();
        let total = self.browser.total();
        let summary = if shown == total {
            format!("{total} items")
        } else {
            format!("{shown} of {total} items")
        };
        let detail = &mut self.type_scale.detail;
        let text_y = y + (theme.status_height - detail.line_height()) / 2;
        detail.draw_text(
            canvas,
            &summary,
            theme.padding,
            text_y,
            theme.muted,
            background,
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
        canvas.clear(self.theme.background);
        self.paint_header(canvas);
        self.paint_sidebar(canvas);
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
                    let y = at.y - self.theme.list_top();
                    if let Some(path) = self
                        .sidebar
                        .row_at(y, self.theme.row_height)
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

    /// A path is truncated from the left, because the tail says where you
    /// are and the head only says you are under /home.
    #[test]
    fn a_long_path_keeps_its_tail() {
        let path = "/home/user/projects/terminal/crates/widget/src";
        let out = FilesWindow::truncate_left(path, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.starts_with('…'));
        assert!(out.ends_with("widget/src"), "got {out}");
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(FilesWindow::truncate_left("/tmp", 20), "/tmp");
    }

    #[test]
    fn degenerate_widths_do_not_panic() {
        assert_eq!(FilesWindow::truncate_left("/some/path", 0), "");
        assert_eq!(FilesWindow::truncate_left("/some/path", 1), "…");
    }

    /// Multi-byte path components must be counted as cells, not bytes.
    #[test]
    fn truncation_counts_characters_not_bytes() {
        let path = "/home/user/日本語のディレクトリ/file";
        let out = FilesWindow::truncate_left(path, 10);
        assert_eq!(out.chars().count(), 10);
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
