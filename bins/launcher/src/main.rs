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

/// The launcher's resolved colors, pulled from `config::Config`'s
/// `[launcher]` table (mirroring `[colors]`/`[font]` when unset) instead of
/// the hardcoded palette the first version shipped with -- same pattern as
/// `bins/monitor`'s `Theme` derived from `config::Config::monitor_*`.
#[derive(Clone, Copy)]
struct Theme {
    bg: Color,
    fg: Color,
    dim: Color,
    highlight_bg: Color,
    /// The window's own background alpha (`0-255`, `255` = opaque), passed
    /// through to every `draw_text_alpha` call so anti-aliased glyph edges
    /// blend correctly against a translucent backbuffer -- "translucent
    /// glass, solid text": ink itself always stays fully opaque regardless
    /// of this value (see `platform::Canvas::blit_glyph`'s doc), but the
    /// edge-blend math needs to know the *real* alpha to composite toward,
    /// not the always-255 default `TextRenderer::draw_text` assumes. Mirrors
    /// `bins/statusbar`'s `PaintStyle::bg_alpha` exactly -- the bug this
    /// field fixes was the launcher never plumbing it at all, so glyph edges
    /// picked up the window's transparency instead of staying solid.
    bg_alpha: u8,
}

impl Theme {
    /// Derive every color this binary paints with from `cfg` in one place,
    /// mirroring `bins/monitor`'s `Theme::from_config` -- `dim` and
    /// `highlight_bg` have no direct `[launcher]` config knob (same as the
    /// monitor dashboard's typography tiers have no `[colors]` equivalent),
    /// so they're derived from the resolved background/foreground instead
    /// of hardcoded literals.
    fn from_config(cfg: &config::Config) -> Self {
        let bg: Color = cfg.launcher_background().into();
        let fg: Color = cfg.launcher_foreground().into();
        // Same float->u8 pixel-alpha conversion as `BarWindow::new` and
        // `LauncherWindow::new` (see either's doc for why the clippy allow
        // is deliberate): clamping to `0.0..=1.0` first guarantees the cast
        // is always in-range, so truncation/sign-loss can't actually occur.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bg_alpha = (cfg.launcher_opacity().clamp(0.0, 1.0) * 255.0).round() as u8;
        let highlight_bg: Color = cfg.launcher_highlight().into();
        Self {
            bg,
            fg,
            dim: Color::rgb(0x6c, 0x70, 0x86),
            highlight_bg,
            bg_alpha,
        }
    }
}

/// The launcher's whole UI state: the query text, the currently selected
/// result, and the catalog it's searching. A fresh [`Catalog::search`] runs
/// every keystroke -- app counts in the hundreds make that cheap enough to
/// not need incremental re-filtering.
struct LauncherApp {
    window: LauncherWindow,
    text: TextRenderer,
    catalog: Catalog,
    theme: Theme,
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
        let mut window =
            match LauncherWindow::new(WINDOW_WIDTH, WINDOW_HEIGHT, cfg.launcher_opacity(), wake) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("launcher: failed to open layer-shell window: {e}");
                    std::process::exit(1);
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

        Self {
            window,
            text,
            catalog: Catalog::load(),
            theme,
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
                theme,
                query,
                selected,
                ..
            } = &mut self;
            match window.tick(|canvas| {
                Painter::paint(canvas, text, catalog, theme, query, *selected);
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
            // `request_redraw` (called from `handle_keys` above whenever a
            // key changed anything) signals the waker itself, so this cap
            // is only a safety net for the rare tick where nothing queued a
            // signal -- keep it short so any missed wake is still
            // imperceptible instead of a visible stall.
            self.window.wait(100);
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
                // Wrap to the last row from the first (and vice versa below)
                // instead of clamping, so repeated Up/Down cycles the list
                // like most launcher/menu UIs rather than dead-ending.
                let count = self
                    .catalog
                    .search(&self.query)
                    .len()
                    .min(MAX_VISIBLE_RESULTS);
                if count > 0 {
                    self.selected = (self.selected + count - 1) % count;
                }
            }
            LauncherKey::Down => {
                let count = self
                    .catalog
                    .search(&self.query)
                    .len()
                    .min(MAX_VISIBLE_RESULTS);
                if count > 0 {
                    self.selected = (self.selected + 1) % count;
                }
            }
            LauncherKey::Enter => self.launch_selected(),
        }
    }

    fn launch_selected(&mut self) {
        let results = self.catalog.search(&self.query);
        if let Some(ranked) = results.get(self.selected)
            && let Some(cmd) = launcher::launch::ExecCommand::for_entry(ranked.entry)
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
        theme: &Theme,
        query: &str,
        selected: usize,
    ) {
        let pad = 20;
        let Theme {
            bg,
            fg,
            dim,
            highlight_bg,
            bg_alpha,
        } = *theme;

        // Query line, with a simple caret-less text field look: just the
        // typed text (or a placeholder) at the top.
        let query_y = pad;
        if query.is_empty() {
            text.draw_text_alpha(canvas, "Type to search…", pad, query_y, dim, bg, bg_alpha);
        } else {
            text.draw_text_alpha(canvas, query, pad, query_y, fg, bg, bg_alpha);
        }

        let line_h = text.line_height() + 10;
        let list_top = query_y + line_h + 10;

        let results = catalog.search(query);
        for (i, ranked) in results.iter().take(MAX_VISIBLE_RESULTS).enumerate() {
            let row_y = list_top + i32::try_from(i).unwrap_or(0) * line_h;
            if i == selected {
                // `fill_rect_alpha`, not `fill_rect`: the launcher window is
                // translucent by default (`launcher_opacity`), so its
                // backbuffer is a premultiplied Argb8888 surface where the
                // top byte is the alpha the compositor reads -- `fill_rect`
                // always packs a zero top byte, which would make the
                // selection highlight invisible instead of just slightly
                // translucent (see fill_rect's own doc for why).
                canvas.fill_rect_alpha(
                    Rect::new(0, row_y - 4, canvas.width(), line_h),
                    highlight_bg,
                    bg_alpha,
                );
            }
            let color = if i == selected { fg } else { dim };
            text.draw_text_alpha(canvas, &ranked.entry.name, pad, row_y, color, bg, bg_alpha);
        }

        if results.is_empty() && !query.is_empty() {
            text.draw_text_alpha(
                canvas,
                "No matching applications",
                pad,
                list_top,
                dim,
                bg,
                bg_alpha,
            );
        }
    }
}

fn main() {
    LauncherApp::new().run();
}
