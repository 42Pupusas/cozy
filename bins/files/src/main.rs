//! `files` -- a Wayland file browser.
//!
//! # Layering
//!
//! ```text
//! fsmodel::DirectoryView   read / filter / sort   (no UI at all)
//! widget::ListPane         scroll / select        (no platform at all)
//! browser::Browser         the state machine      (no pixels at all)
//! window::FilesWindow      paint + route input    (the only Wayland part)
//! ```
//!
//! Each layer is testable without the one above it, which is why the
//! interesting behaviour -- navigating, filtering, landing the cursor back
//! where you came from -- has unit tests that run in microseconds instead of
//! needing a compositor and a pair of eyes.

mod browser;
mod favorites;
mod handlers;
mod keymap;
mod launch;
mod options;
mod row_format;
mod sidebar;
mod test_support;
mod theme;
mod user_dirs;
mod window;

use browser::Browser;
use options::Options;
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
        let theme = Theme::from_config(&cfg);
        let wake = waker::Waker::new()?;

        let size = LogicalSize {
            width: options.width,
            height: options.height,
        };
        // The browser is built with the *logical* size, then corrected the
        // moment the compositor tells us the real physical geometry. Guessing
        // the scale here instead would give a first frame with the wrong
        // number of rows on any HiDPI screen.
        let browser = Browser::open(
            &options.path,
            size.width,
            theme.list_height(size.height),
            theme.row_height,
        )?;

        let mut window = Toplevel::new(
            &browser.path().display().to_string(),
            "dev.terminal.files",
            size,
            wake,
            theme.background,
        )?;

        let mut painter = FilesWindow::new(
            browser,
            theme,
            platform::toplevel::PhysicalSize {
                width: size.width,
                height: theme.list_height(size.height),
            },
        );
        // Adopt the true physical size before the first paint.
        painter.resize(window.size());

        // Loaded once, before the loop: the glob and association databases
        // are a few thousand lines between them, and re-reading disk on
        // every Enter would put that latency directly in the user's way.
        let opener = launch::Opener::load();

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
                opener.open(&path);
            }

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

            window.wait(Self::IDLE_TIMEOUT_MS);
        }
    }

    /// Close the window, mapping its error into the boxed one `main` returns.
    fn finish(window: Toplevel) -> Result<(), Box<dyn std::error::Error>> {
        window.close()?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    App::run()
}
