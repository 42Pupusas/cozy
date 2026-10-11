//! The `xdg_popup` object chain for a popup parented to a layer surface.
//!
//! A layer-shell client cannot name its layer surface as an `xdg_surface`
//! parent, so the popup is created with a null parent and attached through
//! `zwlr_layer_surface_v1.get_popup`. Both that attachment and the grab must
//! reach the compositor before the popup surface's first commit; [`PopupHandle::create`]
//! queues them in that order.
//!
//! Everything is queued on a [`WindowSender`] because popups open while the
//! owning window's event loop is already running.

use crate::conn::WindowSender;
use crate::id::{
    CompositorId, LayerSurfaceId, PopupId, PositionerId, SeatId, SurfaceId, WmBaseId, XdgSurfaceId,
};
use crate::protocol::{
    CompositorRequest, LayerSurfaceRequest, PopupRequest, PositionerAnchor, PositionerGravity,
    PositionerRequest, SurfaceRequest, WmBaseRequest, XdgSurfaceRequest,
};
use crate::wire::MessageBuilder;

/// Placement rules for one popup, in the parent surface's logical
/// coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PopupPositioner {
    pub size: (i32, i32),
    /// `(x, y, width, height)`; width and height must be positive.
    pub anchor_rect: (i32, i32, i32, i32),
    pub anchor: PositionerAnchor,
    pub gravity: PositionerGravity,
    pub offset: (i32, i32),
    /// An OR of [`crate::protocol::ConstraintAdjustment`] bits.
    pub constraint_adjustment: u32,
}

impl PopupPositioner {
    fn send(&self, tx: &mut WindowSender, wm_base: WmBaseId) -> PositionerId {
        let positioner: PositionerId = tx.alloc_id();
        tx.request(MessageBuilder::new(wm_base, WmBaseRequest::CreatePositioner).new_id(positioner));
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetSize)
                .i32(self.size.0)
                .i32(self.size.1),
        );
        let (x, y, w, h) = self.anchor_rect;
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetAnchorRect)
                .i32(x)
                .i32(y)
                .i32(w)
                .i32(h),
        );
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetAnchor).u32(self.anchor as u32),
        );
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetGravity)
                .u32(self.gravity as u32),
        );
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetConstraintAdjustment)
                .u32(self.constraint_adjustment),
        );
        tx.request(
            MessageBuilder::new(positioner, PositionerRequest::SetOffset)
                .i32(self.offset.0)
                .i32(self.offset.1),
        );
        positioner
    }
}

/// The objects a popup is created from and grabbed with.
#[derive(Debug, Clone, Copy)]
pub struct PopupParent {
    pub compositor: CompositorId,
    pub wm_base: WmBaseId,
    pub layer_surface: LayerSurfaceId,
    pub seat: SeatId,
    /// Serial of the input event that opened the popup.
    pub grab_serial: u32,
}

/// One live `wl_surface` → `xdg_surface` → `xdg_popup` chain.
#[derive(Debug, Clone, Copy)]
pub struct PopupHandle {
    pub surface: SurfaceId,
    pub xdg_surface: XdgSurfaceId,
    pub popup: PopupId,
}

impl PopupHandle {
    /// Queue the chain, the layer-surface attachment, the grab, and the
    /// buffer-less commit that solicits the first `xdg_surface.configure`.
    pub fn create(
        tx: &mut WindowSender,
        parent: &PopupParent,
        positioner: &PopupPositioner,
    ) -> Self {
        let positioner_id = positioner.send(tx, parent.wm_base);

        let surface: SurfaceId = tx.alloc_id();
        tx.request(
            MessageBuilder::new(parent.compositor, CompositorRequest::CreateSurface)
                .new_id(surface),
        );
        let xdg_surface: XdgSurfaceId = tx.alloc_id();
        tx.request(
            MessageBuilder::new(parent.wm_base, WmBaseRequest::GetXdgSurface)
                .new_id(xdg_surface)
                .object(surface),
        );
        let popup: PopupId = tx.alloc_id();
        tx.request(
            MessageBuilder::new(xdg_surface, XdgSurfaceRequest::GetPopup)
                .new_id(popup)
                .object(XdgSurfaceId(0))
                .object(positioner_id),
        );
        tx.request(
            MessageBuilder::new(parent.layer_surface, LayerSurfaceRequest::GetPopup)
                .object(popup),
        );
        tx.request(
            MessageBuilder::new(popup, PopupRequest::Grab)
                .object(parent.seat)
                .u32(parent.grab_serial),
        );
        tx.request(MessageBuilder::new(positioner_id, PositionerRequest::Destroy));
        tx.request(MessageBuilder::new(surface, SurfaceRequest::Commit));

        Self {
            surface,
            xdg_surface,
            popup,
        }
    }

    pub fn ack_configure(&self, tx: &WindowSender, serial: u32) {
        tx.request(
            MessageBuilder::new(self.xdg_surface, XdgSurfaceRequest::AckConfigure).u32(serial),
        );
    }

    /// Children before parents: destroying the `xdg_surface` while its
    /// popup role object exists is a protocol error.
    pub fn destroy(&self, tx: &WindowSender) {
        tx.request(MessageBuilder::new(self.popup, PopupRequest::Destroy));
        tx.request(MessageBuilder::new(
            self.xdg_surface,
            XdgSurfaceRequest::Destroy,
        ));
        tx.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::WindowRequest;
    use crate::protocol::{ConstraintAdjustment, Opcode};
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc;

    struct Recorded {
        object: u32,
        opcode: u16,
        first_arg: Option<u32>,
    }

    struct Recorder {
        tx: WindowSender,
        rx: mpsc::Consumer<WindowRequest>,
    }

    impl Recorder {
        fn new() -> Self {
            let (tx, rx) = mpsc::RingBuffer::<WindowRequest>::new(Capacity::exact(64)).split();
            Self {
                tx: WindowSender {
                    tx,
                    next_id: 100,
                    id_end: 200,
                    free_ids: Vec::new(),
                },
                rx,
            }
        }

        fn drain(&mut self) -> Vec<Recorded> {
            let mut out = Vec::new();
            while let Some(request) = self.rx.pop() {
                let WindowRequest::Send(bytes) = request else {
                    panic!("popup requests carry no fds");
                };
                out.push(Recorded {
                    object: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
                    opcode: u16::from_ne_bytes(bytes[4..6].try_into().unwrap()),
                    first_arg: bytes
                        .get(8..12)
                        .map(|b| u32::from_ne_bytes(b.try_into().unwrap())),
                });
            }
            out
        }
    }

    const PARENT: PopupParent = PopupParent {
        compositor: CompositorId(2),
        wm_base: WmBaseId(3),
        layer_surface: LayerSurfaceId(4),
        seat: SeatId(5),
        grab_serial: 77,
    };

    fn positioner() -> PopupPositioner {
        PopupPositioner {
            size: (360, 420),
            anchor_rect: (600, 0, 1, 28),
            anchor: PositionerAnchor::BottomLeft,
            gravity: PositionerGravity::BottomRight,
            offset: (0, 8),
            constraint_adjustment: ConstraintAdjustment::bits(&[ConstraintAdjustment::SlideX]),
        }
    }

    fn position_of(log: &[Recorded], object: u32, opcode: impl Opcode) -> usize {
        let opcode = opcode.opcode();
        log.iter()
            .position(|r| r.object == object && r.opcode == opcode)
            .expect("request was queued")
    }

    #[test]
    fn attachment_and_grab_precede_the_first_commit() {
        let mut rec = Recorder::new();
        let handle = PopupHandle::create(&mut rec.tx, &PARENT, &positioner());
        let log = rec.drain();

        let attach = position_of(&log, 4, LayerSurfaceRequest::GetPopup);
        let grab = position_of(&log, handle.popup.0, PopupRequest::Grab);
        let commit = position_of(&log, handle.surface.0, SurfaceRequest::Commit);
        assert!(attach < commit, "get_popup must precede the first commit");
        assert!(grab < commit, "the grab must precede the first commit");
        assert_eq!(commit, log.len() - 1, "the commit is the last request");
    }

    #[test]
    fn the_positioner_is_complete_before_get_popup_and_destroyed_after() {
        let mut rec = Recorder::new();
        let handle = PopupHandle::create(&mut rec.tx, &PARENT, &positioner());
        let log = rec.drain();

        let created = position_of(&log, 3, WmBaseRequest::CreatePositioner);
        let positioner_id = log[created].first_arg.expect("create_positioner names a new id");
        let offset = position_of(&log, positioner_id, PositionerRequest::SetOffset);
        let get_popup = position_of(&log, handle.xdg_surface.0, XdgSurfaceRequest::GetPopup);
        let destroy = position_of(&log, positioner_id, PositionerRequest::Destroy);
        assert!(offset < get_popup);
        assert!(get_popup < destroy);
    }

    #[test]
    fn destroy_tears_down_children_first() {
        let mut rec = Recorder::new();
        let handle = PopupHandle::create(&mut rec.tx, &PARENT, &positioner());
        rec.drain();
        handle.destroy(&rec.tx);
        let log = rec.drain();

        assert_eq!(log[0].object, handle.popup.0);
        assert_eq!(log[1].object, handle.xdg_surface.0);
        assert_eq!(log[2].object, handle.surface.0);
    }
}
