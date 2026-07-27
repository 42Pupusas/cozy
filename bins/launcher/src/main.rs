//! `launcher` — a centered, keyboard-grabbing app launcher built on the
//! same `io_uring` Wayland connection / software canvas as `terminal` and
//! `statusbar`, docked via `zwlr_layer_shell_v1` like the bar but
//! unanchored (compositor-centered) and keyboard-exclusive.
//!
//! All app-discovery/fuzzy-match/launch logic lives in `crates/launcher`
//! (pure, unit-tested, no Wayland); this binary is just the thin
//! presentation layer: paint the query + result list, react to keys, spawn
//! on Enter, exit on Esc or after a successful launch.

mod icon_cache;
mod launcher_window;

use std::path::PathBuf;

use icon_cache::IconCache;
use launcher::Catalog;
use launcher::singleton::{SingletonAcquire, SingletonGuard};
use launcher_window::{LauncherKey, LauncherWindow};
use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

const WINDOW_WIDTH: i32 = 640;
const WINDOW_HEIGHT: i32 = 520;
const MAX_VISIBLE_RESULTS: usize = 8;
/// Icon glyph edge, in pixels -- square, drawn left of each result row's
/// text with a small gap. Sized to roughly match a result row's text
/// height rather than any XDG icon-theme size, since it's blitted (scaled)
/// from whatever size actually got resolved on disk.
const ICON_SIZE: i32 = 32;

/// The launcher's resolved colors, pulled from `config::Config`'s
/// `[launcher]` table (mirroring `[colors]`/`[font]` when unset) instead of
/// the hardcoded palette the first version shipped with -- same pattern as
/// `bins/monitor`'s `Theme` derived from `config::Config::monitor_*`.
#[derive(Clone, Copy)]
struct Theme {
    bg: Color,
    fg: Color,
    helper: Color,
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
            helper: cfg.launcher_helper().into(),
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
    title_text: TextRenderer,
    command_text: TextRenderer,
    catalog: Catalog,
    icons: IconCache,
    theme: Theme,
    query: String,
    selected: usize,
    /// Set once the user picks an app (Enter) or bails (Esc) -- the next
    /// `run` loop iteration exits instead of ticking again.
    done: bool,
    /// Held for this process's whole lifetime: dropping it (on exit)
    /// unlinks the singleton socket so the next launch can bind it again.
    /// Also polled every loop iteration (`singleton_notified`) -- an
    /// accepted connection means a second launch invocation is asking this
    /// one to close, since a launcher hotkey is a toggle, not "stack a new
    /// overlapping window". See `launcher::singleton` for the full
    /// rationale.
    singleton: SingletonGuard,
}

impl LauncherApp {
    fn new(singleton: SingletonGuard) -> Self {
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
        let title_text = TextRenderer::load_with_fallbacks(
            cfg.launcher_font_path(),
            &[cfg.launcher_emoji_path(), cfg.launcher_icon_path()],
            px_size * 1.15,
        );
        let command_text = TextRenderer::load_with_fallbacks(
            cfg.launcher_font_path(),
            &[cfg.launcher_emoji_path(), cfg.launcher_icon_path()],
            px_size * 0.72,
        );

        Self {
            window,
            text,
            title_text,
            command_text,
            catalog: Catalog::load(),
            icons: IconCache::new(),
            theme,
            query: String::new(),
            selected: 0,
            done: false,
            singleton,
        }
    }

    fn run(mut self) {
        loop {
            if self.done {
                break;
            }
            if self.singleton_notified() {
                self.done = true;
                break;
            }
            let Self {
                window,
                text,
                title_text,
                command_text,
                catalog,
                icons,
                theme,
                query,
                selected,
                ..
            } = &mut self;
            match window.tick(|canvas| {
                Painter::paint(canvas, text, title_text, command_text, catalog, icons, theme, query, *selected);
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

    /// Whether a second launch invocation has connected to our singleton
    /// socket, asking us to close (see [`Self::singleton`]'s doc). The
    /// listener is nonblocking (set once, at construction time in `main`),
    /// so this is a cheap poll -- safe to call every loop iteration
    /// alongside the Wayland tick.
    fn singleton_notified(&self) -> bool {
        self.singleton.listener().accept().is_ok()
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
            LauncherKey::CtrlDigit(digit) => {
                if (1..=9).contains(&digit) {
                    let count = self.catalog.search(&self.query).len().min(MAX_VISIBLE_RESULTS);
                    let index = usize::from(digit - 1);
                    if index < count {
                        self.selected = index;
                        self.launch_selected();
                    }
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
    #[allow(clippy::too_many_arguments)]
    fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        title_text: &mut TextRenderer,
        command_text: &mut TextRenderer,
        catalog: &Catalog,
        icons: &mut IconCache,
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

        // Query line, with a simple caret-less text field look: just the
        // typed text (or a placeholder) at the top.
        let query_y = pad;
        if query.is_empty() {
            text.draw_text_alpha(canvas, "Type to search…", pad, query_y, helper, bg, bg_alpha);
        } else {
            text.draw_text_alpha(canvas, query, pad, query_y, fg, bg, bg_alpha);
        }

        let title_h = title_text.line_height();
        let hint_h = command_text.line_height();
        let row_padding = 12;
        let row_gap = 10;
        let content_h = title_h.max(hint_h).max(ICON_SIZE);
        let line_h = content_h + row_padding * 2;
        let list_top = query_y + text.line_height() + 18;

        let results = catalog.search(query);
        for (i, ranked) in results.iter().take(MAX_VISIBLE_RESULTS).enumerate() {
            let row_top = list_top + i32::try_from(i).unwrap_or(0) * (line_h + row_gap);
            // Both the icon and the text glyphs are drawn from a cell-top
            // origin, but the icon's cell height (`ICON_SIZE`) and the
            // font's own `line_height()` are unrelated numbers -- aligning
            // both to the same `row_top` therefore lines up their *tops*,
            // not their visual centers, which is what actually reads as
            // "aligned" in a row of mixed icon+text. Center each one
            // independently within the row's full height (`line_h`,
            // already the font's line height plus row padding) instead.
            let icon_y = row_top + (line_h - ICON_SIZE) / 2;
            let title_y = row_top + (line_h - title_h) / 2;
            let hint_y = title_y + title_text.baseline_offset() - command_text.baseline_offset();
            if i == selected {
                // `fill_rect_alpha`, not `fill_rect`: the launcher window is
                // translucent by default (`launcher_opacity`), so its
                // backbuffer is a premultiplied Argb8888 surface where the
                // top byte is the alpha the compositor reads -- `fill_rect`
                // always packs a zero top byte, which would make the
                // selection highlight invisible instead of just slightly
                // translucent (see fill_rect's own doc for why).
                canvas.fill_rect_alpha(
                    Rect::new(0, row_top, canvas.width(), line_h),
                    highlight_bg,
                    bg_alpha,
                );
            }
            let row_bg = if i == selected { highlight_bg } else { bg };
            let color = if i == selected { fg } else { helper };
            let command_color = if i == selected { fg } else { helper };

            // Reserve the same icon column for every row, even when an app
            // has no usable icon. This keeps all titles on one vertical axis.
            if let Some(icon_name) = &ranked.entry.icon
                && let Some(icon) = icons.get(icon_name)
            {
                canvas.blit_rgba_scaled(
                    Rect::new(pad, icon_y, ICON_SIZE, ICON_SIZE),
                    &icon.rgba,
                    icon.width,
                    icon.height,
                    row_bg,
                    bg_alpha,
                );
            }
            let text_x = pad + ICON_SIZE + 24;
            title_text.draw_text_alpha(
                canvas,
                &ranked.entry.name,
                text_x,
                title_y,
                color,
                row_bg,
                bg_alpha,
            );
            if i < 9 {
                let hint = format!("Ctrl+{}", i + 1);
                command_text.draw_text_right_alpha(
                    canvas,
                    &hint,
                    canvas.width() - pad,
                    hint_y,
                    command_color,
                    row_bg,
                    bg_alpha,
                );
            }
        }

        if results.is_empty() && !query.is_empty() {
            text.draw_text_alpha(
                canvas,
                "No matching applications",
                pad,
                list_top,
                helper,
                bg,
                bg_alpha,
            );
        }
    }
}

/// The singleton socket's path -- `XDG_RUNTIME_DIR` (the same
/// per-user, per-session runtime dir Wayland/D-Bus sockets live in;
/// tmpfs-backed and cleared on logout, so a leftover socket from a
/// previous session can never outlive its own login) falling back to
/// `/tmp` if unset, matching how `platform::conn` resolves the Wayland
/// socket itself.
fn singleton_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    dir.join("launcher.sock")
}

fn main() {
    let path = singleton_socket_path();
    let guard = match SingletonGuard::acquire(&path) {
        Ok(SingletonAcquire::Primary(guard)) => guard,
        Ok(SingletonAcquire::AlreadyRunning) => {
            // A running instance was notified to close (toggle behavior) --
            // this invocation's job is done.
            return;
        }
        Err(e) => {
            eprintln!(
                "launcher: failed to acquire singleton lock at {}: {e} — continuing without single-instance protection",
                path.display()
            );
            // Degrade gracefully rather than refuse to launch at all: fall
            // through with a throwaway listener bound wherever the OS picks,
            // so `LauncherApp` still has a `SingletonGuard` to hold
            // (`UnixListener::bind("")` isn't valid, so this uses a
            // process-unique fallback path under /tmp instead).
            let fallback = std::env::temp_dir().join(format!("launcher-{}.sock", std::process::id()));
            match SingletonGuard::acquire(&fallback) {
                Ok(SingletonAcquire::Primary(guard)) => guard,
                _ => return,
            }
        }
    };
    if let Err(e) = guard.listener().set_nonblocking(true) {
        eprintln!("launcher: failed to set singleton socket nonblocking: {e}");
    }
    LauncherApp::new(guard).run();
}
