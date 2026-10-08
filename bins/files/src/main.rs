//! `files` -- a Wayland file browser.
//!
//! # Layering
//!
//! ```text
//! fsmodel::DirectoryView   read / filter / sort   (no UI at all)
//! widget::ListPane         cursor / select        (no platform at all)
//! browser::Browser         the state machine      (no pixels at all)
//! tile_field::TileField    rank / spiral layout   (no pixels at all)
//! window::FilesWindow      paint + route input    (the only Wayland part)
//! ```
//!
//! Each layer is testable without the one above it, which is why the
//! interesting behaviour -- navigating, filtering, landing the cursor back
//! where you came from -- has unit tests that run in microseconds instead of
//! needing a compositor and a pair of eyes.

mod breadcrumb;
mod browser;
mod clipboard;
mod density;
mod dir_sizes;
mod dirs_watch;
mod drag_badge;
mod favorites;
mod frame;
mod headless;
mod history;
mod hover;
mod keymap;
mod metric_overrides;
mod mount_service;
mod mount_session;
mod options;
mod password_modal;
mod password_panel;
mod pinned_card;
mod rename;
mod row_format;
mod row_kind;
mod secret_input;
mod sidebar;
mod spacing;
mod spiral;
mod spiral_cursor;
mod storage;
mod test_support;
mod theme;
mod tile_field;
mod tile_painter;
mod tile_rank;
mod toolbar;
mod typography;
mod window;

use browser::Browser;
use options::Options;
use platform::toplevel::activation_report::ActivationReport;
use platform::toplevel::{LogicalSize, Toplevel, WindowEvent};
use theme::Theme;
use window::FilesWindow;

/// Runs the browser: owns the window, the event loop, and the exit path.
///
/// A struct rather than a long `main` so the loop's phases are named and the
/// teardown has an obvious place to live.
struct App;

impl App {
    /// How long to sleep when nothing is happening.
    ///
    /// The window is entirely event-driven -- it repaints only when something
    /// changed -- so this timeout exists purely as a safety net against a
    /// missed wakeup, not as a frame clock. A file browser that spun at 60fps
    /// showing a static list would be a laptop-battery bug.
    const IDLE_TIMEOUT_MS: i32 = 250;

    fn run() -> Result<(), Box<dyn std::error::Error>> {
        let options = Options::parse(std::env::args().skip(1))?;
        // A broken config should not stop the browser opening -- report it
        // and fall back to defaults, exactly as the launcher and bar do.
        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("files: config error: {e} — using defaults");
            config::Config::default()
        });
        // The type scale is loaded before the theme's metrics are finalized:
        // row/header/status heights derive from the tiers' line heights, so a
        // larger configured font grows the chrome instead of clipping it.
        // Scale 1.0 here; the compositor's real output scale is applied when
        // the window reports its physical size below.
        let type_scale = typography::Typography::load(&cfg, 1.0);
        // Three sources, in priority order: an explicit `[files]` key, then
        // the guide's geometry scaled to the configured font, then a floor
        // from the loaded type scale so no band is shorter than the glyphs it
        // must hold. `with_spacing` runs first because it owns the chrome
        // heights the guide also sizes the controls against; `with_metrics`
        // then only raises a band that would clip, and never lowers one.
        let theme = Theme::from_config(&cfg)
            .with_spacing(spacing::Spacing::new(cfg.font.px_size(), 1.0))
            .with_metrics(
                type_scale.natural_row_height(),
                type_scale.natural_header_height(),
                type_scale.natural_status_height(),
            );
        let favorites = favorites::Favorites::load();
        let user_dirs = user_dirs::UserDirs::load();
        let sidebar = sidebar::Sidebar::new(&user_dirs, &favorites);
        let wake = waker::Waker::new()?;

        let size = LogicalSize {
            width: options.width,
            height: options.height,
        };
        let field = frame::Frame::new(
            size.width,
            size.height,
            sidebar.visible(),
            theme.spacing,
            theme.card_metrics(),
        )
        .field();
        let browser = Browser::open(&options.path, field.w, field.h, theme.row_height)?;

        // The browser is as translucent as the terminal, bar, launcher and
        // monitor -- `[files].opacity` mirrors `[window].opacity`, so one
        // desktop-wide setting moves every client together and the browser
        // does not sit among them as the one opaque window.
        let surface = platform::SurfaceFormat::from_opacity(cfg.files_opacity());
        // Cloned before the toplevel takes ownership: the mount worker signals
        // the same eventfd the loop sleeps on, which is what makes a password
        // prompt appear the instant polkit asks rather than at the next
        // unrelated event.
        let mount_wake = wake.clone();
        let mut window = Toplevel::new_with_format(
            &browser.path().display().to_string(),
            "cozy.files",
            size,
            wake,
            theme.background,
            surface,
        )?;

        let mut painter = FilesWindow::new(
            browser,
            theme,
            type_scale,
            sidebar,
            favorites,
            platform::toplevel::PhysicalSize {
                width: size.width,
                height: size.height,
            },
            mount_wake,
        );
        // Adopt the true physical size before the first paint.
        painter.resize(window.size());

        // Loaded once, before the loop: the glob and association databases
        // are a few thousand lines between them, and re-reading disk on
        // every Enter would put that latency directly in the user's way.
        let mut opener = opener::Opener::load();
        let activation_report = ActivationReport::from_env("files");

        let mut title = painter.title();
        loop {
            window.tick(&mut painter)?;

            while let Some(event) = window.next_event() {
                match event {
                    WindowEvent::CloseRequested => return Self::finish(window),
                    WindowEvent::Resized(size) => {
                        painter.resize(size);
                        window.request_redraw();
                    }
                }
            }

            if let Some(path) = painter.take_pending_open() {
                // Deliberately outside the paint callback: spawning a process
                // mid-frame would run it while the framebuffer is borrowed.
                //
                // The token is minted here, per open, because it is spent by
                // the launch it authorises: it carries the serial of the
                // click that asked for this file, so the compositor lets the
                // handler's window come to the user rather than treating it
                // as focus stealing.
                let token = window.activation_token(None);
                activation_report.record(token.as_ref(), window.supports_activation());
                opener.set_activation(
                    token.map(|token| opener::ActivationToken::new(token.into_string())),
                );
                opener.open(&path);
            }

            // Same reasoning as `take_pending_open`: starting a drag talks
            // to the `Toplevel`, which `Painter::on_pointer` cannot reach.
            if let Some(paths) = painter.take_pending_drag() {
                window.start_drag(&paths);
            }

            // The drop-target highlight depends on `Toplevel`-owned drag
            // state (`is_drag_over`/`drag_position`), so it is reported to
            // the painter here rather than through a callback -- exactly the
            // seam `start_drag` above needs in the other direction.
            painter.update_drag_hover(window.is_drag_over(), window.drag_position());
            // The compositor holds the pointer grab for the whole gesture, so
            // the release never reaches the painter. This is the only signal
            // that the drag ended, and without it the badge never clears.
            painter.update_drag_active(window.is_dragging());
            if window.is_drag_over() || window.is_dragging() {
                window.request_redraw();
            }

            Self::poll_storage(&mut window, &mut painter);

            // The title follows the directory, so the taskbar entry stays
            // meaningful as the user navigates. Compared first because
            // set_title is a round-trip we should not make every frame.
            let current = painter.title();
            if current != title {
                window.set_title(&current);
                title = current;
            }

            if painter.should_quit() || window.is_closing() {
                return Self::finish(window);
            }

            Self::idle_wait(&window, &painter);
        }
    }

    /// Sleep until the compositor, the directory watcher, or the idle
    /// timeout wakes the loop.
    ///
    /// [`Toplevel::wait`] can only sleep on its own eventfd; with a live
    /// directory watch the loop must also wake when files change, so both
    /// fds go through the same `poll(2)` primitive the portal loop uses to
    /// multiplex its bus socket. The waker is drained here, exactly as
    /// [`Toplevel::wait`] drains it, so an armed counter cannot turn every
    /// later wait into an instant return; the counter semantics still
    /// preserve any signal that raced in during the pass.
    fn idle_wait(window: &Toplevel, painter: &FilesWindow) {
        let timeout = window.wait_timeout_ms(Self::IDLE_TIMEOUT_MS);
        match painter.watch_fd() {
            Some(watch_fd) => {
                let (waker_ready, _) =
                    waker::Poller::two(window.waker().raw_fd(), watch_fd).poll_two(timeout);
                if waker_ready {
                    window.waker().drain();
                }
                // The watcher side is read by `poll_storage` on the next
                // pass; nothing to do with the readiness bit here.
            }
            None => {
                window.wait(timeout);
            }
        }
    }

    /// Poll the sources that produce work without any input: devices, the
    /// mount worker, the folder-size worker behind the tiles, and the
    /// directory watcher behind the listing.
    ///
    /// Each is pulled once per pass, and none is a response to an event:
    ///
    /// * Devices attach and detach on their own. That poll is interval-gated
    ///   inside, so an ordinary pass costs a clock read.
    /// * A mount runs on a worker thread, and its password prompt **blocks
    ///   that thread** until this call collects it. A skipped pass is a
    ///   worker left waiting and a modal that never appears, so this is the
    ///   one poll here that must not be gated on anything.
    /// * The directory watcher reports files other processes created,
    ///   renamed or deleted; the re-read goes through the browser, so the
    ///   cursor follows the file rather than the row number.
    ///
    /// All are called unconditionally, and the results combined afterwards:
    /// `||` short-circuits, so a device change would skip the mount poll
    /// entirely and strand the worker for as long as devices kept changing.
    fn poll_storage(window: &mut Toplevel, painter: &mut FilesWindow) {
        let devices = painter.poll_volumes();
        let mount = painter.poll_mount();
        let sizes = painter.poll_tiles();
        let listing = painter.poll_listing();
        if devices || mount || sizes || listing {
            window.request_redraw();
        }
    }

    /// Close the window, mapping its error into the boxed one `main` returns.
    fn finish(window: Toplevel) -> Result<(), Box<dyn std::error::Error>> {
        window.close()?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if headless::FileStress::maybe_run(&arguments)? {
        return Ok(());
    }
    App::run()
}
