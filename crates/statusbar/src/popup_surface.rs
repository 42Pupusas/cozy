//! A panel popup surface living on the bar's Wayland connection: the
//! `xdg_popup` chain, its own double-buffered shm swap chain, and the
//! configure / `popup_done` handshake.

use platform::Result;
use platform::conn::WindowSender;
use platform::draw::{Canvas, Color};
use platform::event::RawEvent;
use platform::id::{ShmId, SurfaceId};
use platform::popup::{PopupHandle, PopupParent, PopupPositioner};
use platform::present::{PresentAttempt, PresentState};
use platform::protocol::{Format, PopupEvent, XdgSurfaceEvent};
use platform::surface::{self, Frame, Routed};

/// What the popup is filled with before the panel paints.
#[derive(Debug, Clone, Copy)]
pub struct PopupStyle {
    pub background: Color,
    pub bg_alpha: u8,
}

pub struct PopupSurface {
    handle: PopupHandle,
    present: PresentState,
    width: i32,
    height: i32,
    style: PopupStyle,
    configured: bool,
    dismissed: bool,
}

impl PopupSurface {
    pub fn open(
        tx: &mut WindowSender,
        shm: ShmId,
        parent: &PopupParent,
        positioner: &PopupPositioner,
        style: PopupStyle,
    ) -> Result<Self> {
        let (width, height) = positioner.size;
        let format = if style.bg_alpha == 255 {
            Format::Xrgb8888
        } else {
            Format::Argb8888
        };
        let (frames, shm_mem, pool) = Frame::create_pair_queued(tx, shm, width, height, format)?;
        let handle = PopupHandle::create(tx, parent, positioner);
        Ok(Self {
            handle,
            present: PresentState::new(frames, shm_mem, pool, format, 1),
            width,
            height,
            style,
            configured: false,
            dismissed: false,
        })
    }

    #[must_use]
    pub const fn surface(&self) -> SurfaceId {
        self.handle.surface
    }

    /// The compositor withdrew the popup (outside click, lost grab).
    #[must_use]
    pub const fn is_dismissed(&self) -> bool {
        self.dismissed
    }

    pub const fn request_redraw(&mut self) {
        self.present.mark_dirty();
    }

    /// Handle `ev` if it concerns this popup; `Ok(false)` passes it on.
    pub fn handle_event(&mut self, ev: &RawEvent, tx: &mut WindowSender) -> Result<bool> {
        match self.present.route_common(ev, tx) {
            Routed::Fatal(e) => return Err(e),
            Routed::Handled => return Ok(true),
            Routed::Passthrough => {}
        }
        if ev.is(self.handle.xdg_surface) {
            if ev.event::<XdgSurfaceEvent>() == Some(XdgSurfaceEvent::Configure) {
                self.handle.ack_configure(tx, ev.arg(0));
                self.configured = true;
                self.present.mark_dirty();
            }
            return Ok(true);
        }
        if ev.is(self.handle.popup) {
            if ev.event::<PopupEvent>() == Some(PopupEvent::PopupDone) {
                self.dismissed = true;
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn present(&mut self, tx: &mut WindowSender, paint: &mut impl FnMut(&mut Canvas)) {
        if !self.configured || self.dismissed {
            return;
        }
        let PresentAttempt::Ready(slot) = self.present.begin_present() else {
            return;
        };
        let bg = surface::SurfaceBootstrap::pack_bg(self.style.background, self.style.bg_alpha);
        self.present.repaint_slot(
            slot,
            (self.width, self.height),
            platform::SurfaceFormat::with_bg_alpha(self.style.bg_alpha),
            bg,
            paint,
        );
        surface::SurfacePresenter::commit_full_frame(
            &mut self.present,
            tx,
            self.handle.surface,
            slot,
            self.width,
            self.height,
        );
    }

    /// Destroy the role chain, then the buffers and pool behind it.
    pub fn close(self, tx: &WindowSender) {
        self.handle.destroy(tx);
        self.present.destroy(tx);
    }
}
