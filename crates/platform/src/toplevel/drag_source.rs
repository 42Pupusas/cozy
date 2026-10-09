//! The outgoing half of a drag: the `wl_data_source` we publish while
//! dragging files *out* of this window.
//!
//! # Why this is its own type
//!
//! [`crate::toplevel::drag::DragController`] used to own both directions.
//! They share a wire vocabulary but not a lifecycle or a failure mode: an
//! outgoing source is created once per gesture, negotiates an action, serves
//! one `send`, and dies; an incoming offer is created by the *peer*, and we
//! only ever consume it. Folding both into one type made "is this fd request
//! ours to answer" a runtime guess.
//!
//! # The action negotiation, and why a drag is dead without it
//!
//! From `wl_data_device_manager` version 3 the compositor picks a single
//! `dnd_action` by intersecting the source's mask with the destination's. A
//! source that never calls `set_actions` has an empty mask, so the
//! intersection is empty -- and a compositor then withholds
//! `wl_data_device.drop` from the destination entirely. The target still
//! receives `enter` and the offered MIME types, so it paints its "drop here"
//! affordance, and then nothing arrives when the button is released. That is
//! not a subtle degradation; it is the whole feature failing while looking
//! like it works.
//!
//! # Why only `copy`
//!
//! `move` obliges the *source* to delete the transferred files once the
//! destination reports `dnd_finished`. This type does not delete anything,
//! so advertising `move` would promise a peer something we do not honour.
//! Offering `copy` alone keeps the negotiation truthful and still lets every
//! drop target accept the drag. Moving a file within this window is the
//! destination's own business (see `Dropped`) and needs no action bit.

use crate::conn::WindowSender;
use crate::id::{DataDeviceId, DataDeviceManagerId, DataSourceId, SurfaceId};
use crate::protocol::{
    DataDeviceManagerRequest, DataDeviceRequest, DataSourceEvent, DataSourceRequest, DndAction,
};
use crate::wire::MessageBuilder;

/// The MIME type a file drag is offered as -- the universally recognised
/// "here are some files" format every drop target already understands.
/// See [`crate::file_uri`] for the encoding.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// A `wl_data_source` carrying dragged files, for as long as one gesture
/// lasts.
pub struct DragSource {
    /// The live source object while a drag we started is in flight.
    id: Option<DataSourceId>,
    /// The `text/uri-list` body served to the drop target on `send`.
    body: String,
    /// Set when a `data_source.send` just arrived; the next `RawEvent::Fd`
    /// is the pipe to write [`Self::body`] into.
    awaiting_send_fd: bool,
    /// The action the compositor last selected for this drag, from
    /// `wl_data_source.action`. [`DndAction::None`] means the negotiation
    /// has not settled on anything, in which case the drop will not be
    /// delivered -- see the module docs.
    selected_action: DndAction,
    /// Whether the peer completed the transfer (`dnd_finished`) rather than
    /// abandoning it (`cancelled`).
    finished: bool,
}

impl DragSource {
    /// An idle source: no drag in flight.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            id: None,
            body: String::new(),
            awaiting_send_fd: false,
            selected_action: DndAction::None,
            finished: false,
        }
    }

    /// Publish `paths` as a `text/uri-list` source and start the drag from
    /// `origin` under `serial`.
    ///
    /// The request order is fixed by the protocol: create the source, offer
    /// its MIME type, declare the action mask, and only then `start_drag`.
    /// `set_actions` may be sent once and must precede `start_drag`.
    ///
    /// No-op if a drag is already live -- a second `start_drag` while one is
    /// in flight is a protocol error.
    pub fn start(
        &mut self,
        window_tx: &mut WindowSender,
        manager: DataDeviceManagerId,
        device: DataDeviceId,
        origin: SurfaceId,
        serial: u32,
        paths: &[std::path::PathBuf],
    ) {
        if paths.is_empty() || self.id.is_some() {
            return;
        }
        self.body = crate::file_uri::UriList::encode(paths);
        self.selected_action = DndAction::None;
        self.finished = false;

        let src: DataSourceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(manager, DataDeviceManagerRequest::CreateDataSource).new_id(src),
        );
        window_tx.request(MessageBuilder::new(src, DataSourceRequest::Offer).string(URI_LIST_MIME));
        window_tx.request(
            MessageBuilder::new(src, DataSourceRequest::SetActions).u32(DndAction::Copy.mask()),
        );
        // Wire arg order matches the XML declaration: source, origin, icon
        // (nullable -- 0 means no drag icon), serial.
        window_tx.request(
            MessageBuilder::new(device, DataDeviceRequest::StartDrag)
                .object(src)
                .object(origin)
                .u32(0)
                .u32(serial),
        );
        self.id = Some(src);
    }

    /// Whether a drag we started is currently live.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.id.is_some()
    }

    /// The live source's object id, for the window's dispatch to route
    /// `wl_data_source.*` events here.
    #[must_use]
    pub const fn id(&self) -> Option<DataSourceId> {
        self.id
    }

    /// The action the compositor selected for this drag.
    #[must_use]
    pub const fn selected_action(&self) -> DndAction {
        self.selected_action
    }

    /// Whether the last drag ended with the destination consuming it.
    #[must_use]
    pub const fn did_finish(&self) -> bool {
        self.finished
    }

    /// Handle one `wl_data_source` event. `action_bits` is the event's first
    /// argument, meaningful only for [`DataSourceEvent::Action`].
    ///
    /// `Send` arms the awaiting-fd flag (the fd arrives as the next
    /// `RawEvent::Fd`). `DndDropPerformed` means the button was released but
    /// says nothing about acceptance, so the source stays alive. `Cancelled`
    /// and `DndFinished` both end the drag -- no one accepted it, or a
    /// destination consumed it.
    pub const fn on_event(&mut self, event: DataSourceEvent, action_bits: u32) {
        match event {
            DataSourceEvent::Send => self.awaiting_send_fd = true,
            DataSourceEvent::Action => {
                self.selected_action = DndAction::from_selected(action_bits);
            }
            DataSourceEvent::DndFinished => {
                self.finished = true;
                self.id = None;
            }
            DataSourceEvent::Cancelled => self.id = None,
            _ => {}
        }
    }

    /// Whether a `data_source.send` is in flight, i.e. the next
    /// `RawEvent::Fd` is the pipe we should write our body into.
    #[must_use]
    pub const fn is_awaiting_send_fd(&self) -> bool {
        self.awaiting_send_fd
    }

    /// Write the outgoing `text/uri-list` body into `fd` (a peer's pipe from
    /// a `data_source.send`), then let it close on scope exit to signal EOF.
    pub fn write_send_fd(&mut self, fd: std::os::fd::OwnedFd) {
        use crate::sys::OwnedFdExt as _;
        self.awaiting_send_fd = false;
        fd.write_all_and_close(self.body.as_bytes());
    }
}

impl Default for DragSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ObjectId as _;

    fn test_sender() -> WindowSender {
        use quetzalcoatl::capacity::Capacity;
        use quetzalcoatl::mpsc;
        let (tx, _rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        WindowSender {
            tx,
            next_id: 100,
            id_end: 200,
            free_ids: Vec::new(),
        }
    }

    fn started() -> (DragSource, WindowSender) {
        let mut src = DragSource::new();
        let mut tx = test_sender();
        src.start(
            &mut tx,
            DataDeviceManagerId(1),
            DataDeviceId(2),
            SurfaceId(3),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        (src, tx)
    }

    #[test]
    fn a_fresh_source_is_idle() {
        let src = DragSource::new();
        assert!(!src.is_active());
        assert_eq!(src.id(), None);
    }

    #[test]
    fn starting_with_no_paths_is_a_no_op() {
        let mut src = DragSource::new();
        let mut tx = test_sender();
        src.start(
            &mut tx,
            DataDeviceManagerId(1),
            DataDeviceId(2),
            SurfaceId(3),
            7,
            &[],
        );
        assert!(!src.is_active());
    }

    #[test]
    fn starting_publishes_a_source() {
        let (src, _tx) = started();
        assert!(src.is_active());
        assert!(src.id().is_some());
    }

    /// The bug this module exists to prevent: a drag must declare an action
    /// mask, or the compositor never delivers `drop` to the destination.
    #[test]
    fn starting_declares_the_copy_action_before_the_drag_begins() {
        use crate::conn::WindowRequest;
        use quetzalcoatl::capacity::Capacity;
        use quetzalcoatl::mpsc;

        let (tx, mut rx) = mpsc::RingBuffer::<WindowRequest>::new(Capacity::exact(64)).split();
        let mut window_tx = WindowSender {
            tx,
            next_id: 100,
            id_end: 200,
            free_ids: Vec::new(),
        };
        let mut src = DragSource::new();
        src.start(
            &mut window_tx,
            DataDeviceManagerId(1),
            DataDeviceId(2),
            SurfaceId(3),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );

        // Keyed on `(object, opcode)`: an opcode alone is ambiguous, since
        // it numbers requests within one interface and several here share
        // the value 0.
        let source_id = src.id().expect("a source was created").raw();
        let mut sent = Vec::new();
        while let Some(WindowRequest::Send(msg)) = rx.pop() {
            sent.push((
                u32::from_ne_bytes(msg[0..4].try_into().expect("object")),
                u16::from_ne_bytes(msg[4..6].try_into().expect("opcode")),
            ));
        }
        let set_actions = sent
            .iter()
            .position(|&m| m == (source_id, DataSourceRequest::SetActions as u16))
            .expect("set_actions must be sent on the source");
        let start_drag = sent
            .iter()
            .position(|&m| m == (2, DataDeviceRequest::StartDrag as u16))
            .expect("start_drag must be sent on the device");
        assert!(
            set_actions < start_drag,
            "set_actions must precede start_drag, or the compositor rejects it"
        );
    }

    #[test]
    fn a_second_start_while_one_is_live_is_ignored() {
        let (mut src, mut tx) = started();
        let first = src.id();
        src.start(
            &mut tx,
            DataDeviceManagerId(1),
            DataDeviceId(2),
            SurfaceId(3),
            8,
            &[std::path::PathBuf::from("/b.txt")],
        );
        assert_eq!(src.id(), first, "must not replace a live drag");
    }

    #[test]
    fn send_arms_the_awaiting_fd_flag() {
        let (mut src, _tx) = started();
        assert!(!src.is_awaiting_send_fd());
        src.on_event(DataSourceEvent::Send, 0);
        assert!(src.is_awaiting_send_fd());
    }

    #[test]
    fn the_compositors_chosen_action_is_recorded() {
        let (mut src, _tx) = started();
        assert_eq!(src.selected_action(), DndAction::None);
        src.on_event(DataSourceEvent::Action, DndAction::Copy.mask());
        assert_eq!(src.selected_action(), DndAction::Copy);
    }

    #[test]
    fn a_drop_performed_does_not_end_the_drag() {
        // Acceptance is still unknown at this point; `cancelled` may follow.
        let (mut src, _tx) = started();
        src.on_event(DataSourceEvent::DndDropPerformed, 0);
        assert!(src.is_active());
    }

    #[test]
    fn cancelled_ends_the_drag_unfinished() {
        let (mut src, _tx) = started();
        src.on_event(DataSourceEvent::Cancelled, 0);
        assert!(!src.is_active());
        assert!(!src.did_finish());
    }

    #[test]
    fn dnd_finished_ends_the_drag_as_completed() {
        let (mut src, _tx) = started();
        src.on_event(DataSourceEvent::DndFinished, 0);
        assert!(!src.is_active());
        assert!(src.did_finish());
    }

    #[test]
    fn restarting_clears_the_previous_drags_negotiation() {
        let (mut src, mut tx) = started();
        src.on_event(DataSourceEvent::Action, DndAction::Copy.mask());
        src.on_event(DataSourceEvent::DndFinished, 0);
        src.start(
            &mut tx,
            DataDeviceManagerId(1),
            DataDeviceId(2),
            SurfaceId(3),
            9,
            &[std::path::PathBuf::from("/b.txt")],
        );
        assert_eq!(src.selected_action(), DndAction::None);
        assert!(!src.did_finish());
    }

    #[test]
    fn the_served_body_is_the_encoded_uri_list() {
        let (src, _tx) = started();
        assert_eq!(src.body, "file:///a.txt\r\n");
    }
}
