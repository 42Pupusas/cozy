//! The one panel popup the bar may have open, parented to its layer
//! surface.
//!
//! niri hands a layer-surface popup's grab keyboard focus only while the
//! parent layer surface accepts it, so opening switches the bar to
//! `OnDemand` interactivity (committed before the grab is requested) and
//! closing switches it back to `None`.

use platform::conn::WindowSender;
use platform::draw::Canvas;
use platform::event::RawEvent;
use platform::id::{CompositorId, SeatId, ShmId, SurfaceId, WmBaseId};
use platform::layer::LayerSurfaceHandle;
use platform::popup::{PopupParent, PopupPositioner};
use platform::protocol::{KeyboardInteractivity, SurfaceRequest, WmBaseEvent, WmBaseRequest};
use platform::wire::MessageBuilder;
use platform::Result;

use crate::popup_surface::{PopupStyle, PopupSurface};

pub struct BarPopup {
    compositor: CompositorId,
    shm: ShmId,
    wm_base: WmBaseId,
    seat: SeatId,
    current: Option<PopupSurface>,
}

impl BarPopup {
    #[must_use]
    pub const fn new(compositor: CompositorId, shm: ShmId, wm_base: WmBaseId, seat: SeatId) -> Self {
        Self {
            compositor,
            shm,
            wm_base,
            seat,
            current: None,
        }
    }

    #[must_use]
    pub fn surface(&self) -> Option<SurfaceId> {
        self.current.as_ref().map(PopupSurface::surface)
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.current.is_some()
    }

    #[must_use]
    pub fn is_dismissed(&self) -> bool {
        self.current.as_ref().is_some_and(PopupSurface::is_dismissed)
    }

    pub const fn request_redraw(&mut self) {
        if let Some(popup) = &mut self.current {
            popup.request_redraw();
        }
    }

    /// Replace any open popup with a new one grabbed by the press `serial`.
    pub fn open(
        &mut self,
        tx: &mut WindowSender,
        layer: LayerSurfaceHandle,
        serial: u32,
        positioner: &PopupPositioner,
        style: PopupStyle,
    ) -> Result<()> {
        self.close(tx, layer);
        layer.set_keyboard_interactivity_queued(tx, KeyboardInteractivity::OnDemand);
        tx.request(MessageBuilder::new(layer.surface, SurfaceRequest::Commit));
        let parent = PopupParent {
            compositor: self.compositor,
            wm_base: self.wm_base,
            layer_surface: layer.layer_surface,
            seat: self.seat,
            grab_serial: serial,
        };
        self.current = Some(PopupSurface::open(tx, self.shm, &parent, positioner, style)?);
        Ok(())
    }

    pub fn close(&mut self, tx: &WindowSender, layer: LayerSurfaceHandle) {
        let Some(popup) = self.current.take() else {
            return;
        };
        popup.close(tx);
        layer.set_keyboard_interactivity_queued(tx, KeyboardInteractivity::None);
        tx.request(MessageBuilder::new(layer.surface, SurfaceRequest::Commit));
    }

    /// Handle `ev` if it is a shell ping or concerns the open popup.
    pub fn handle_event(&mut self, ev: &RawEvent, tx: &mut WindowSender) -> Result<bool> {
        if ev.is(self.wm_base) {
            if ev.event::<WmBaseEvent>() == Some(WmBaseEvent::Ping) {
                tx.request(MessageBuilder::new(self.wm_base, WmBaseRequest::Pong).u32(ev.arg(0)));
            }
            return Ok(true);
        }
        self.current
            .as_mut()
            .map_or(Ok(false), |popup| popup.handle_event(ev, tx))
    }

    pub fn present(&mut self, tx: &mut WindowSender, paint: &mut impl FnMut(&mut Canvas)) {
        if let Some(popup) = &mut self.current {
            popup.present(tx, paint);
        }
    }
}
