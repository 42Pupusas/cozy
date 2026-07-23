//! `launcher` — a centered, keyboard-grabbing app launcher built on the
//! same `io_uring` Wayland connection / software canvas as `terminal` and
//! `statusbar`, docked via `zwlr_layer_shell_v1` like the bar but
//! unanchored (compositor-centered) and keyboard-exclusive.
//!
//! All app-discovery/fuzzy-match/launch logic lives in `crates/launcher`
//! (pure, unit-tested, no Wayland); this binary is just the thin
//! presentation layer: paint the query + result list, react to keys, spawn
//! on Enter, exit on Esc or after a successful launch.

mod launcher_window;

use launcher::Catalog;
use launcher_window::{LauncherKey, LauncherWindow};
use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

const WINDOW_WIDTH: i32 = 640;
const WINDOW_HEIGHT: i32 = 400;
const MAX_VISIBLE_RESULTS: usize = 8;

/// The launcher's whole UI state: the query text, the currently selected
/// result, and the catalog it's searching. A fresh [`Catalog::search`] runs
/// every keystroke -- app counts in the hundreds make that cheap enough to
/// not need incremental re-filtering.
struct LauncherApp {
    window: LauncherWindow,
    text: TextRenderer,
    catalog: Catalog,
    query: String,
    selected: usize,
    /// Set once the user picks an app (Enter) or bails (Esc) -- the next
    /// `run` loop iteration exits instead of ticking again.
    done: bool,
}

impl LauncherApp {
    fn new() -> Self {
        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("launcher: config error: {e} — using defaults");
            config::Config::default()
        });

        let wake = waker::Waker::new().expect("create waker");
        let mut window = match LauncherWindow::new(WINDOW_WIDTH, WINDOW_HEIGHT, wake) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("launcher: failed to open layer-shell window: {e}");
                std::process::exit(1);
            }
        };

        let bg: Color = cfg.bar_background().into();
        window.set_background(bg);

        let scale_f32 = f32::from(i16::try_from(window.scale()).unwrap_or(i16::MAX));
        let px_size = cfg.bar_font_px_size() * scale_f32;
        let text = TextRenderer::load_with_fallbacks(
            cfg.bar_font_path(),
            &[cfg.bar_emoji_path(), cfg.bar_icon_path()],
            px_size,
        );

        Self {
            window,
            text,
            catalog: Catalog::load(),
            query: String::new(),
            selected: 0,
            done: false,
        }
    }

    fn run(mut self) {
        loop {
            if self.done {
                break;
            }
            let Self {
                window,
                text,
                catalog,
                query,
                selected,
                ..
            } = &mut self;
            match window.tick(|canvas| {
                Painter::paint(canvas, text, catalog, query, *selected);
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
                // One last tick so the window's teardown request (in
                // `Drop`) goes out over an already-flushed connection --
                // matches every other window's exit path in this
                // workspace.
                break;
            }
            self.window.wait(1000);
        }
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
                self.selected = self.selected.saturating_sub(1);
            }
            LauncherKey::Down => {
                let count = self.catalog.search(&self.query).len();
                if self.selected + 1 < count.min(MAX_VISIBLE_RESULTS) {
                    self.selected += 1;
                }
            }
            LauncherKey::Enter => self.launch_selected(),
        }
    }

    fn launch_selected(&mut self) {
        let results = self.catalog.search(&self.query);
        if let Some(ranked) = results.get(self.selected)
            && let Some(cmd) = launcher::launch::ExecCommand::parse(&ranked.entry.exec)
            && let Err(e) = cmd.spawn_detached()
        {
            eprintln!("launcher: failed to launch {:?}: {e}", ranked.entry.name);
        }
        self.done = true;
    }
}

/// The launcher's per-frame paint pass -- a marker type grouping
/// [`Self::paint`], mirroring `statusbar`'s own `Painter`.
struct Painter;

impl Painter {
    fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        catalog: &Catalog,
        query: &str,
        selected: usize,
    ) {
        let pad = 20;
        let fg = Color::rgb(0xcd, 0xd6, 0xf4);
        let dim = Color::rgb(0x6c, 0x70, 0x86);
        let highlight_bg = Color::rgb(0x31, 0x32, 0x44);
        let bg = Color::rgb(0x1e, 0x1e, 0x2e);

        // Query line, with a simple caret-less text field look: just the
        // typed text (or a placeholder) at the top.
        let query_y = pad;
        if query.is_empty() {
            text.draw_text(canvas, "Type to search…", pad, query_y, dim, bg);
        } else {
            text.draw_text(canvas, query, pad, query_y, fg, bg);
        }

        let line_h = text.line_height() + 10;
        let list_top = query_y + line_h + 10;

        let results = catalog.search(query);
        for (i, ranked) in results.iter().take(MAX_VISIBLE_RESULTS).enumerate() {
            let row_y = list_top + i32::try_from(i).unwrap_or(0) * line_h;
            if i == selected {
                canvas.fill_rect(Rect::new(0, row_y - 4, canvas.width(), line_h), highlight_bg);
            }
            let color = if i == selected { fg } else { dim };
            text.draw_text(canvas, &ranked.entry.name, pad, row_y, color, bg);
        }

        if results.is_empty() && !query.is_empty() {
            text.draw_text(canvas, "No matching applications", pad, list_top, dim, bg);
        }
    }
}

fn main() {
    LauncherApp::new().run();
}
