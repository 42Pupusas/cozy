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
use crate::row_format::RowFormat;
use crate::theme::Theme;

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
    pub const fn new(browser: Browser, theme: Theme, size: PhysicalSize) -> Self {
        Self {
            browser,
            theme,
            size,
            focused: false,
            quit: false,
            pending_open: None,
            last_click: None,
        }
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
        self.browser
            .resize(size.width, self.theme.list_height(size.height));
    }

    /// Fold an [`Outcome`] into window state.
    fn apply(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Quit => self.quit = true,
            Outcome::Open(path) => self.pending_open = Some(path),
            Outcome::Ignored | Outcome::Redraw => {}
        }
    }

    /// Convert a pointer position into list-pane coordinates.
    ///
    /// Returns `None` above or below the list, so a click on the header or
    /// status bar cannot be mistaken for a click on whichever row happens to
    /// line up with it.
    const fn to_pane(&self, at: Position) -> Option<(i32, i32)> {
        let y = at.y - self.theme.list_top();
        if y < 0 || y >= self.theme.list_height(self.size.height) {
            return None;
        }
        Some((at.x, y))
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

    /// How many character cells fit in `width` pixels.
    ///
    /// Negative widths are real: a window dragged narrower than its own
    /// padding produces one, and a sign-losing cast would turn it into a
    /// column count of two billion.
    fn columns(width: i32) -> usize {
        usize::try_from(width / platform::font::GLYPH_W).unwrap_or(0)
    }

    /// Draw the path header.
    fn paint_header(&self, canvas: &mut Canvas<'_>) {
        let theme = &self.theme;
        canvas.fill_rect(
            Rect::new(0, 0, self.size.width, theme.header_height),
            theme.header_bg,
        );
        // Vertically centre the 16px glyph cell in the header strip.
        let text_y = (theme.header_height - platform::font::GLYPH_H) / 2;
        let width = self.size.width - theme.padding * 2;
        let path = self.browser.path().display().to_string();
        // Truncated from the *left*: the tail of a path (where you are) is
        // far more useful than the head (that you are under /home).
        let columns = Self::columns(width);
        let shown = Self::truncate_left(&path, columns);
        canvas.draw_text(&shown, theme.padding, text_y, theme.text);
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
    fn paint_rows(&self, canvas: &mut Canvas<'_>) {
        let theme = &self.theme;
        let pane = self.browser.pane();
        let top = theme.list_top();
        let content_width = pane.content_width();
        // The size column is right-aligned in a fixed gutter, so names in a
        // list of mixed sizes still line up.
        let size_cells = 7;
        let size_width = size_cells * platform::font::GLYPH_W;
        let name_width = (content_width - size_width - theme.padding * 3).max(0);
        let name_columns = Self::columns(name_width);

        for row in pane.visible_rows() {
            let Some(y) = pane.row_y(row) else { continue };
            let Some(entry) = self.browser.entry(row) else {
                continue;
            };
            let y = top + y;
            let rect = Rect::new(0, y, content_width, theme.row_height);

            if pane.is_selected(row) {
                canvas.fill_rect(rect, theme.selection);
            }
            // The cursor outline is drawn even on an unselected row, and only
            // when focused: an outline on an unfocused window claims a
            // keyboard focus we do not have.
            if self.focused && pane.cursor() == Some(row) {
                canvas.draw_rect(rect, theme.cursor);
            }

            let text_y = y + (theme.row_height - platform::font::GLYPH_H) / 2;
            let name = RowFormat::truncate(&RowFormat::name(entry), name_columns);
            canvas.draw_text(&name, theme.padding, text_y, RowFormat::color(entry, theme));

            let size = RowFormat::size(entry);
            // Right-align by padding the string rather than by measuring
            // pixels: the font is fixed-width, so the two are the same thing
            // and this survives a font-size change.
            let size_x = content_width
                - theme.padding
                - i32::try_from(size.chars().count()).unwrap_or(0) * platform::font::GLYPH_W;
            canvas.draw_text(&size, size_x, text_y, theme.muted);
        }
    }

    /// Draw the scrollbar, if the list overflows.
    fn paint_scrollbar(&self, canvas: &mut Canvas<'_>) {
        let pane = self.browser.pane();
        if !pane.scrollbar_visible() {
            return;
        }
        let theme = &self.theme;
        let x = pane.gutter_x();
        let track_h = theme.list_height(self.size.height);
        let gutter = self.size.width - x;
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
    fn paint_status(&self, canvas: &mut Canvas<'_>) {
        let theme = &self.theme;
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
        let text_y = y + (theme.status_height - platform::font::GLYPH_H) / 2;

        // An error outranks everything else: it explains why the list is not
        // what the user expected, and hiding it behind a file count would
        // leave them staring at a stale listing wondering what happened.
        if let Some(error) = self.browser.error() {
            canvas.draw_text(&error.message(), theme.padding, text_y, theme.error);
            return;
        }

        if filtering {
            let prefix = "filter: ";
            canvas.draw_text(prefix, theme.padding, text_y, theme.muted);
            let query_x =
                theme.padding + i32::try_from(prefix.len()).unwrap_or(0) * platform::font::GLYPH_W;
            let query = self.browser.filter().text();
            canvas.draw_text(query, query_x, text_y, theme.text);
            // The caret sits at the character offset of the byte caret, which
            // is why the count is over chars before it rather than bytes.
            let caret_cells = self.browser.filter().before_caret().chars().count();
            let caret_x =
                query_x + i32::try_from(caret_cells).unwrap_or(0) * platform::font::GLYPH_W;
            canvas.fill_rect(
                Rect::new(caret_x, text_y, 2, platform::font::GLYPH_H),
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
        canvas.draw_text(&summary, theme.padding, text_y, theme.muted);
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

    #[test]
    fn columns_divide_the_width_by_the_glyph_cell() {
        assert_eq!(FilesWindow::columns(platform::font::GLYPH_W * 10), 10);
        assert_eq!(FilesWindow::columns(0), 0);
        assert_eq!(
            FilesWindow::columns(-100),
            0,
            "a negative width is no columns"
        );
    }
}
