use std::os::fd::RawFd;

use platform::SurfaceFormat;
use platform::toplevel::{LogicalSize, Toplevel, WindowEvent};
use portal_core::{ChooserDialog, ChooserOutcome, ChooserRequest};
use tiles::tile_style::TileStyle;
use waker::Waker;

use crate::chooser_model::ChooserModel;
use crate::chooser_view::ChooserView;
use crate::deadline::Deadline;

pub struct ChooserWindow {
    window: Option<Toplevel>,
    view: ChooserView,
    waker: Waker,
}

impl ChooserWindow {
    const APP_ID: &'static str = "cozy.portal.chooser";
    const SIZE: LogicalSize = LogicalSize {
        width: 900,
        height: 600,
    };

    /// # Errors
    ///
    /// Returns a message when the request is not an open request, the start folder cannot be read, or the Wayland window cannot be set up.
    pub fn open(request: &ChooserRequest) -> Result<Self, String> {
        let model = ChooserModel::open(request)?;
        let cfg = config::Config::load().unwrap_or_default();
        let TileStyle { theme, type_scale } = TileStyle::from_config(&cfg);
        let waker = Waker::new().map_err(|error| error.to_string())?;
        let window = Toplevel::new_with_format(
            &request.title,
            Self::APP_ID,
            Self::SIZE,
            waker.clone(),
            theme.background,
            SurfaceFormat::from_opacity(cfg.files_opacity()),
        )
        .map_err(|error| error.to_string())?;
        let mut view = ChooserView::new(model, theme, type_scale, window.size(), waker.clone());
        view.resize(window.size());
        Ok(Self {
            window: Some(window),
            view,
            waker,
        })
    }

    fn end(&mut self, outcome: ChooserOutcome) -> ChooserOutcome {
        self.shut_window();
        outcome
    }

    fn shut_window(&mut self) {
        if let Some(window) = self.window.take()
            && let Err(error) = window.close()
        {
            eprintln!("portal: closing the chooser window failed: {error}");
        }
    }

    fn drain_window_events(window: &mut Toplevel, view: &mut ChooserView) -> bool {
        let mut closed = false;
        while let Some(event) = window.next_event() {
            match event {
                WindowEvent::CloseRequested => closed = true,
                WindowEvent::Resized(size) => {
                    view.resize(size);
                    window.request_redraw();
                }
            }
        }
        closed || window.is_closing()
    }
}

impl ChooserDialog for ChooserWindow {
    fn wake_fd(&self) -> RawFd {
        self.waker.raw_fd()
    }

    fn watch_fds(&self) -> Vec<RawFd> {
        self.view.watch_fd().into_iter().collect()
    }

    fn wait_ms(&self) -> Option<i32> {
        let window = self.window.as_ref()?;
        Deadline::soonest(window.repeat_wait_ms(), self.view.motion_wait_ms())
    }

    fn advance(&mut self) -> Option<ChooserOutcome> {
        self.waker.drain();
        let window = self.window.as_mut()?;
        if self.view.poll_listing() | self.view.poll_tiles() | self.view.advance_motion() {
            window.request_redraw();
        }
        if let Err(error) = window.tick(&mut self.view) {
            eprintln!("portal: chooser window failed: {error}");
            return Some(self.end(ChooserOutcome::Failed));
        }
        let closed = Self::drain_window_events(window, &mut self.view);
        if let Some(outcome) = self.view.take_outcome() {
            return Some(self.end(outcome));
        }
        if closed {
            return Some(self.end(ChooserOutcome::Cancelled));
        }
        None
    }

    fn close(&mut self) {
        self.shut_window();
    }
}
