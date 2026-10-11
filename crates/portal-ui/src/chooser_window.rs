use std::os::fd::RawFd;

use platform::toplevel::{LogicalSize, Painter, Toplevel, WindowEvent};
use portal_core::{ChooserDialog, ChooserOutcome, ChooserRequest};
use waker::Waker;

use crate::chooser_painter::ChooserPainter;

pub struct ChooserWindow {
    window: Option<Toplevel>,
    painter: ChooserPainter,
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
    /// Returns a message when the Wayland connection or the window cannot be set up.
    pub fn open(request: &ChooserRequest) -> Result<Self, String> {
        let waker = Waker::new().map_err(|error| error.to_string())?;
        let painter = ChooserPainter::new();
        let window = Toplevel::new(
            &request.title,
            Self::APP_ID,
            Self::SIZE,
            waker.clone(),
            painter.background(),
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            window: Some(window),
            painter,
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

    fn closed_by_user(window: &mut Toplevel) -> bool {
        let mut closed = false;
        while let Some(event) = window.next_event() {
            match event {
                WindowEvent::CloseRequested => closed = true,
                WindowEvent::Resized(_) => window.request_redraw(),
            }
        }
        closed || window.is_closing()
    }
}

impl ChooserDialog for ChooserWindow {
    fn wake_fd(&self) -> RawFd {
        self.waker.raw_fd()
    }

    fn advance(&mut self) -> Option<ChooserOutcome> {
        self.waker.drain();
        let window = self.window.as_mut()?;
        if let Err(error) = window.tick(&mut self.painter) {
            eprintln!("portal: chooser window failed: {error}");
            return Some(self.end(ChooserOutcome::Failed));
        }
        if self.painter.cancel_requested() || Self::closed_by_user(window) {
            return Some(self.end(ChooserOutcome::Cancelled));
        }
        None
    }

    fn close(&mut self) {
        self.shut_window();
    }
}
