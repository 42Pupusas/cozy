//! The `--dmenu` presentation layer: pick one of stdin's lines, print it.
//!
//! Sibling to `LauncherApp` (the `.desktop` app picker) sharing the same
//! `LauncherWindow`, but with three deliberate differences:
//!   * **no singleton lock** -- the app launcher is a toggle bound to a
//!     hotkey, so a second press should dismiss the first window. A dmenu
//!     invocation is one step of a shell pipeline with a caller blocked on
//!     its stdout; silently exiting because another menu happens to be open
//!     would hand that caller an empty answer.
//!   * **no icon column** -- choices are arbitrary text, not apps.
//!   * **selection goes to stdout**, not `exec()`.
//!
//! Its own module rather than more branches inside `main.rs`: the two modes
//! share only the window, and interleaving them would leave every paint and
//! key handler asking "which mode am I in?".

use launcher::dmenu::ChoiceList;
use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;

use crate::Theme;
use crate::launcher_window::{LauncherKey, LauncherWindow};

const WINDOW_WIDTH: i32 = 720;
const WINDOW_HEIGHT: i32 = 520;
const MAX_VISIBLE_RESULTS: usize = 10;

/// The dmenu UI's whole state: the choices, the query, and the cursor.
pub struct DmenuApp {
    window: LauncherWindow,
    text: TextRenderer,
    choice_text: TextRenderer,
    choices: ChoiceList,
    theme: Theme,
    query: String,
    selected: usize,
    done: bool,
    /// The line to print on exit, set only by Enter.
    ///
    /// `None` on Esc, and the distinction is the whole contract: a caller
    /// pipes our stdout into something that acts on it, so cancelling must
    /// produce *no* output rather than a plausible-looking line. `xargs -r`
    /// on the other end then correctly does nothing.
    picked: Option<String>,
}

impl DmenuApp {
    /// Build the menu over `choices`. Returns `None` if the window can't be
    /// opened, so `main` can exit non-zero without printing a selection.
    pub fn new(choices: ChoiceList, prompt: String) -> Option<Self> {
        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("launcher: config error: {e} — using defaults");
            config::Config::default()
        });

        let wake = waker::Waker::new().expect("create waker");
        let mut window =
            match LauncherWindow::new(WINDOW_WIDTH, WINDOW_HEIGHT, cfg.launcher_opacity(), wake) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("launcher: failed to open layer-shell window: {e}");
                    return None;
                }
            };

        let theme = Theme::from_config(&cfg);
        window.set_background(theme.bg);

        let scale_f32 = f32::from(i16::try_from(window.scale()).unwrap_or(i16::MAX));
        let px_size = cfg.launcher_font_px_size() * scale_f32;
        let text = TextRenderer::load_with_fallbacks(
            cfg.launcher_font_path(),
            &[cfg.launcher_emoji_path(), cfg.launcher_icon_path()],
            px_size,
        );
        // Choices render a touch smaller than the query line: menu lines
        // are frequently long (clipboard previews run to ~72 chars), so the
        // extra horizontal room matters more here than display weight.
        let choice_text = TextRenderer::load_with_fallbacks(
            cfg.launcher_font_path(),
            &[cfg.launcher_emoji_path(), cfg.launcher_icon_path()],
            px_size * 0.92,
        );

        Some(Self {
            window,
            text,
            choice_text,
            choices,
            theme,
            query: prompt,
            selected: 0,
            done: false,
            picked: None,
        })
    }

    /// Run until the user picks or cancels, returning the chosen line.
    pub fn run(mut self) -> Option<String> {
        loop {
            if self.done {
                break;
            }
            let Self {
                window,
                text,
                choice_text,
                choices,
                theme,
                query,
                selected,
                ..
            } = &mut self;
            match window.tick(|canvas| {
                DmenuPainter::paint(canvas, text, choice_text, choices, theme, query, *selected);
            }) {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    eprintln!("launcher: {e}");
                    break;
                }
            }

            self.handle_keys();
            if self.done {
                break;
            }
            self.window.wait(-1);
        }
        self.picked
    }

    fn handle_keys(&mut self) {
        let keys = self.window.take_keys();
        if keys.is_empty() {
            return;
        }
        for key in keys {
            self.apply_key(key);
        }
        self.window.request_redraw();
    }

    /// How many rows are currently on screen -- the bound every cursor
    /// movement wraps against.
    fn visible_count(&self) -> usize {
        self.choices
            .search(&self.query)
            .len()
            .min(MAX_VISIBLE_RESULTS)
    }

    fn apply_key(&mut self, key: LauncherKey) {
        match key {
            LauncherKey::Char(c) => {
                self.query.push(c);
                self.selected = 0;
            }
            LauncherKey::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            LauncherKey::Esc => self.done = true,
            LauncherKey::Up => {
                let count = self.visible_count();
                if count > 0 {
                    self.selected = (self.selected + count - 1) % count;
                }
            }
            LauncherKey::Down => {
                let count = self.visible_count();
                if count > 0 {
                    self.selected = (self.selected + 1) % count;
                }
            }
            LauncherKey::CtrlDigit(digit) => {
                if (1..=9).contains(&digit) {
                    let index = usize::from(digit - 1);
                    if index < self.visible_count() {
                        self.selected = index;
                        self.pick_selected();
                    }
                }
            }
            LauncherKey::Enter => self.pick_selected(),
        }
    }

    fn pick_selected(&mut self) {
        // Re-run the search rather than caching it: `picked` must be the
        // line under the cursor *now*, and the query may have changed since
        // the last paint.
        let results = self.choices.search(&self.query);
        if let Some(ranked) = results.get(self.selected) {
            self.picked = Some(ranked.choice.to_owned());
        }
        self.done = true;
    }
}

/// The dmenu paint pass -- a marker type grouping [`Self::paint`], mirroring
/// `main.rs`'s `Painter` and `statusbar`'s.
struct DmenuPainter;

impl DmenuPainter {
    fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        choice_text: &mut TextRenderer,
        choices: &ChoiceList,
        theme: &Theme,
        query: &str,
        selected: usize,
    ) {
        let pad = 20;
        let Theme {
            bg,
            fg,
            helper,
            highlight_bg,
            bg_alpha,
        } = *theme;

        let query_y = pad;
        if query.is_empty() {
            text.draw_text_alpha(
                canvas,
                "Type to filter…",
                pad,
                query_y,
                helper,
                bg,
                bg_alpha,
            );
        } else {
            text.draw_text_alpha(canvas, query, pad, query_y, fg, bg, bg_alpha);
        }

        let row_padding = 8;
        let row_gap = 6;
        let line_h = choice_text.line_height() + row_padding * 2;
        let list_top = query_y + text.line_height() + 18;

        let results = choices.search(query);
        for (i, ranked) in results.iter().take(MAX_VISIBLE_RESULTS).enumerate() {
            let row_top = list_top + i32::try_from(i).unwrap_or(0) * (line_h + row_gap);
            let row_y = row_top + (line_h - choice_text.line_height()) / 2;
            if i == selected {
                // Solid highlight; the canvas knows the window is translucent
                // -- see `main.rs`'s Painter.
                canvas.fill_rect(Rect::new(0, row_top, canvas.width(), line_h), highlight_bg);
            }
            let row_bg = if i == selected { highlight_bg } else { bg };
            let color = if i == selected { fg } else { helper };
            // Tabs are a field separator in these pipelines (the clipboard
            // history emits `id<TAB>preview`), not something to render as
            // whitespace of unknown width -- show a space so the row reads
            // as one line while the *stored* choice keeps its real tab for
            // the caller's `cut -f1`.
            let display = ranked.choice.replace('\t', "  ");
            choice_text.draw_text_alpha(canvas, &display, pad, row_y, color, row_bg, bg_alpha);
        }

        if results.is_empty() {
            let msg = if choices.is_empty() {
                "Nothing to choose from"
            } else {
                "No matches"
            };
            text.draw_text_alpha(canvas, msg, pad, list_top, helper, bg, bg_alpha);
        }
    }
}
