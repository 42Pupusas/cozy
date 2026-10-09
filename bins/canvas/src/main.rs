//! `canvas` -- an infinite-canvas diagram tool.
//!
//! Excalidraw-lite: pan and zoom over an unbounded plane, add text notes
//! and simple shapes (rect, ellipse, arrow), move and resize them, undo.
//! The document persists to `$XDG_DATA_HOME/terminal/canvas.json`.
//!
//! # Layering
//!
//! ```text
//! shape::Shape        geometry + hit-testing      (no UI at all)
//! doc::Doc            shape list + undo history   (no platform at all)
//! camera::Camera      pan/zoom viewport           (no platform at all)
//! edit_note           multiline caret editing     (no platform at all)
//! session::Session    the interaction state machine (no pixels at all)
//! window::CanvasWindow  paint + route input       (the only Wayland part)
//! ```

mod camera;
mod doc;
mod double_click;
mod edit_note;
mod handle;
mod headless;
mod icon;
mod panel;
mod persistence;
mod session;
mod shape;
mod text_range;
mod theme;
mod window;

use platform::SurfaceFormat;
use platform::toplevel::{LogicalSize, Toplevel, WindowEvent};

use camera::Camera;
use doc::Doc;
use session::Session;
use theme::Theme;
use window::CanvasWindow;

/// Runs the canvas: owns the window, the event loop, and the exit path.
struct App;

impl App {
    fn run() -> Result<(), Box<dyn std::error::Error>> {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        if headless::CanvasStress::maybe_run(&arguments)? {
            return Ok(());
        }

        let cfg = config::Config::load().unwrap_or_else(|e| {
            eprintln!("canvas: config error: {e} — using defaults");
            config::Config::default()
        });
        let store = persistence::Store::new();
        let (doc, camera) = store.load().unwrap_or_else(|e| {
            eprintln!("canvas: {e} — starting empty");
            (Doc::default(), Camera::default())
        });

        let wake = waker::Waker::new()?;
        let size = LogicalSize {
            width: 1024,
            height: 720,
        };
        // As translucent as the terminal/bar/launcher/monitor/files, so a
        // desktop-wide opacity setting moves every client together.
        let surface = SurfaceFormat::from_opacity(cfg.canvas_opacity());
        let theme = Theme::from_config(&cfg, 1.0);
        let mut window = Toplevel::new_with_format(
            "canvas",
            "cozy.canvas",
            size,
            wake,
            theme.background,
            surface,
        )?;

        // The real theme is scaled once the output scale is known; the
        // placeholder above sized the window's initial buffers.
        let scale = window.scale();
        let scale_f32 = f32::from(i16::try_from(scale).unwrap_or(1));
        let theme = Theme::from_config(&cfg, scale_f32);
        let font_px = cfg.font.px_size() * scale_f32;
        let text = statusbar::TextRenderer::load_with_fallbacks(
            cfg.font.path.as_deref(),
            &[cfg.font.emoji_path.as_deref()],
            font_px,
        );
        let session = Session::new(doc, camera, theme.snap);
        let mut painter = CanvasWindow::new(session, theme, text, scale);

        loop {
            window.tick(&mut painter)?;

            // The painter decides the shape; only the window may talk to
            // the compositor, so the request is handed over here.
            if let Some(shape) = painter.take_cursor() {
                window.set_cursor(shape.key(), shape.theme_names());
            }

            while let Some(event) = window.next_event() {
                if matches!(event, WindowEvent::CloseRequested) {
                    return Self::finish(window, &mut painter);
                }
            }

            if painter.should_quit() || window.is_closing() {
                return Self::finish(window, &mut painter);
            }

            window.wait(window.repeat_wait_ms());
        }
    }

    /// Persist if dirty, then close.
    fn finish(
        window: Toplevel,
        painter: &mut CanvasWindow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        painter.session.close_note();
        if painter.session.is_dirty() {
            let store = persistence::Store::new();
            match store.save(&painter.session.doc, &painter.session.camera) {
                Ok(()) => painter.session.mark_saved(),
                Err(e) => eprintln!("canvas: {e}"),
            }
        }
        window.close()?;
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    App::run()
}
