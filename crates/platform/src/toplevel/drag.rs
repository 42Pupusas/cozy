//! Drag-and-drop over `wl_data_device`, for a [`super::window::Toplevel`].
//!
//! # Why one mechanism serves both directions
//!
//! A file browser needs to drag files *into* other windows (dropping onto a
//! browser upload field, a chat client) and *within* itself (dropping a file
//! onto a folder row to move it). Those look like different features, but
//! `wl_data_device.start_drag` grabs the pointer for the compositor -- our
//! own surface receives `enter`/`motion`/`drop` on its data device exactly
//! like any other drop target would, if the pointer happens to still be over
//! us when the button is released. So an in-window move and a drop onto
//! another application are the *same* protocol exchange from this type's
//! point of view; only the payload (a `text/uri-list` of the dragged paths)
//! and what the recipient does with it differ, and both are the caller's
//! business, not this controller's.
//!
//! # Split from [`crate::clipboard::ClipboardState`]
//!
//! Clipboard and drag-and-drop share a wire vocabulary (`wl_data_source`,
//! `wl_data_offer`) but not a lifecycle: a clipboard selection is set once
//! and outlives the app that set it, while a drag is scoped to one pointer
//! gesture and must be torn down the moment it ends (`dnd_finished`,
//! `cancelled`, or the offer's `leave`). Folding both into one type would
//! make "is this fd request a paste or a drop" a runtime guess; keeping them
//! apart means neither has to reason about the other's state at all.
//!
//! # Why the drop payload is read on a helper thread
//!
//! Once `receive` is requested, the peer writes the payload into our pipe
//! and closes it at its own pace -- there is no bound on how long that
//! takes, and it happens on a different process's schedule entirely. A
//! synchronous read here would block the whole event loop (no repaints, no
//! other input) for as long as the peer takes, exactly the bug
//! [`crate::clipboard::ClipboardState::request_paste`] avoids the same way:
//! hand the read to a helper thread and let the window poll for the result.
//!
//! # Testing note
//!
//! Like [`crate::clipboard::ClipboardState`], the outgoing half (start a
//! drag, answer `send`) and the incoming bookkeeping (`enter`/`leave`/offered
//! MIME types) are exercised directly. The pipe read itself is exercised
//! with a real `UnixStream` pair standing in for the peer.

use crate::conn::WindowSender;
use crate::id::{
    DataDeviceId, DataDeviceManagerId, DataOfferId, DataSourceId, ObjectId, SeatId, SurfaceId,
};
use crate::protocol::{
    DataDeviceManagerRequest, DataDeviceRequest, DataOfferEvent, DataOfferRequest, DataSourceEvent,
    DataSourceRequest,
};
use crate::wire::MessageBuilder;

/// The MIME type a file drag is offered as -- the universally recognised
/// "here are some files" format every drop target already understands.
/// See `crate::file_uri` for the encoding.
pub const URI_LIST_MIME: &str = "text/uri-list";

/// Where a drag that landed on us came from, once the payload has arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    /// The `text/uri-list` body, already decoded to paths.
    pub paths: Vec<std::path::PathBuf>,
    /// Surface-local logical position of the drop -- the last position
    /// reported by `wl_data_device.motion` before the drop landed. Lets the
    /// caller resolve *which row* (which target directory) the files were
    /// dropped onto: during a `wl_data_device` grab, ordinary `wl_pointer`
    /// events stop arriving for the surface, so this is the only position
    /// information the drop carries.
    pub position: (i32, i32),
}

/// Drag-and-drop state for one seat's `wl_data_device`.
///
/// Mirrors [`crate::clipboard::ClipboardState`]'s shape deliberately: same
/// `Option`-means-unavailable convention, same "every method a no-op without
/// a manager" contract, so a compositor lacking `wl_data_device_manager`
/// simply has no drag-and-drop rather than a special-cased startup path.
pub struct DragController {
    /// `(manager, per-seat device)` if the compositor supports
    /// `wl_data_device`. `None` disables drag-and-drop entirely.
    data_device: Option<(DataDeviceManagerId, DataDeviceId)>,
    /// Our outgoing source while a drag we started is live.
    source: Option<DataSourceId>,
    /// The `text/uri-list` body served to the drop target on `send`.
    outgoing_body: String,
    /// An incoming drag's offer, from the moment its `enter` arrives until
    /// its `leave` or `drop`.
    incoming_offer: Option<DataOfferId>,
    /// MIME types the current incoming offer advertised. A drag carrying no
    /// `text/uri-list` (e.g. dragging selected text from another app) is
    /// tracked so a window can still show a "cannot accept" affordance
    /// instead of treating every hovering drag identically.
    incoming_mime_types: Vec<String>,
    /// The most recent surface-local position from `wl_data_device.enter` or
    /// `.motion`, for a window to resolve a drop target (see [`Dropped::position`]).
    incoming_position: (i32, i32),
    /// Set when a `data_source.send` just arrived; the next `RawEvent::Fd`
    /// is the pipe to write our outgoing body into.
    awaiting_send_fd: bool,
    /// Drop channel: the helper thread spawned by [`Self::take_drop`] sends
    /// the decoded payload here once the peer finishes writing; the UI loop
    /// drains it in `tick`. Mirrors [`crate::clipboard::ClipboardState`]'s
    /// paste channel exactly, and for the same reason (see module docs).
    drop_tx: std::sync::mpsc::Sender<Dropped>,
    drop_rx: std::sync::mpsc::Receiver<Dropped>,
}

impl DragController {
    /// Build drag-and-drop state from an already-bound `(manager, device)`
    /// pair (or `None` if the compositor never advertised a manager).
    ///
    /// Takes the pair rather than binding its own device: a seat has
    /// exactly one `wl_data_device`, so if a caller already bound one (e.g.
    /// a clipboard implementation) a second `get_data_device` for the same
    /// seat would be a protocol misuse, not a second independent device.
    /// [`Self::bind`] is the constructor for a `Toplevel`, which owns no
    /// clipboard and so binds this device itself.
    #[must_use]
    pub fn new(data_device: Option<(DataDeviceManagerId, DataDeviceId)>) -> Self {
        let (drop_tx, drop_rx) = std::sync::mpsc::channel();
        Self {
            data_device,
            source: None,
            outgoing_body: String::new(),
            incoming_offer: None,
            incoming_mime_types: Vec::new(),
            incoming_position: (0, 0),
            awaiting_send_fd: false,
            drop_tx,
            drop_rx,
        }
    }

    /// Build drag-and-drop state, binding a fresh `wl_data_device` for
    /// `seat` if the compositor advertised `manager`. `None` disables
    /// drag-and-drop entirely (rare, but a compositor is not required to
    /// support it).
    #[must_use]
    pub fn bind(
        manager: Option<DataDeviceManagerId>,
        seat: SeatId,
        window_tx: &mut WindowSender,
    ) -> Self {
        let data_device = manager.map(|mgr| {
            let dev: DataDeviceId = window_tx.alloc_id();
            window_tx.request(
                MessageBuilder::new(mgr, DataDeviceManagerRequest::GetDataDevice)
                    .new_id(dev)
                    .object(seat),
            );
            (mgr, dev)
        });
        Self::new(data_device)
    }

    /// The bound `wl_data_device` object id, for routing incoming events.
    #[must_use]
    pub const fn device_id(&self) -> Option<DataDeviceId> {
        match self.data_device {
            Some((_, dev)) => Some(dev),
            None => None,
        }
    }

    /// Start dragging `paths` from `origin` (the surface the press happened
    /// on), under `serial` (the press's serial -- `wl_data_device.start_drag`
    /// requires an implicit pointer grab matching one).
    ///
    /// No-op if there's no data device, `paths` is empty, or a drag is
    /// already in flight: starting a second `start_drag` while one is live
    /// is a protocol error, and an empty payload has nothing for a drop
    /// target to receive.
    pub fn start_drag(
        &mut self,
        window_tx: &mut WindowSender,
        origin: SurfaceId,
        serial: u32,
        paths: &[std::path::PathBuf],
    ) {
        if paths.is_empty() || self.source.is_some() {
            return;
        }
        let Some((mgr, dev)) = self.data_device else {
            return;
        };
        self.outgoing_body = crate::file_uri::UriList::encode(paths);
        let src: DataSourceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(mgr, DataDeviceManagerRequest::CreateDataSource).new_id(src),
        );
        window_tx.request(MessageBuilder::new(src, DataSourceRequest::Offer).string(URI_LIST_MIME));
        // Wire arg order matches the XML declaration: source, origin, icon
        // (nullable -- 0 means no drag icon), serial.
        window_tx.request(
            MessageBuilder::new(dev, DataDeviceRequest::StartDrag)
                .object(src)
                .object(origin)
                .u32(0)
                .u32(serial),
        );
        self.source = Some(src);
    }

    /// Whether a drag we started is currently live.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.source.is_some()
    }

    /// The current outgoing source id, if any -- for the window's dispatch
    /// to route `wl_data_source.*` events here.
    #[must_use]
    pub const fn source_id(&self) -> Option<DataSourceId> {
        self.source
    }

    /// Handle a `wl_data_source` event for our outgoing drag source.
    /// `Send` arms the awaiting-fd flag (the fd itself arrives as the next
    /// `RawEvent::Fd`); `Cancelled` and `DndFinished` both end the drag --
    /// cancelled means no one accepted it, finished means a drop target
    /// consumed it, and either way there is nothing left to hold open.
    pub const fn on_source_event(&mut self, event: DataSourceEvent) {
        match event {
            DataSourceEvent::Send => self.awaiting_send_fd = true,
            DataSourceEvent::Cancelled | DataSourceEvent::DndFinished => self.source = None,
            _ => {}
        }
    }

    /// Whether a `data_source.send` is in flight, i.e. the next
    /// `RawEvent::Fd` is the pipe we should write our outgoing body into.
    #[must_use]
    pub const fn is_awaiting_send_fd(&self) -> bool {
        self.awaiting_send_fd
    }

    /// Write our outgoing `text/uri-list` body into `fd` (a peer's pipe from
    /// a `data_source.send`), then let it close on scope exit to signal EOF.
    pub fn write_send_fd(&mut self, fd: std::os::fd::OwnedFd) {
        use std::io::Write as _;
        use std::os::fd::FromRawFd as _;
        self.awaiting_send_fd = false;
        let raw = std::os::fd::IntoRawFd::into_raw_fd(fd);
        let mut f = unsafe { std::fs::File::from_raw_fd(raw) };
        let _ = f.write_all(self.outgoing_body.as_bytes());
    }

    /// Handle a `wl_data_device.enter` event during an incoming drag:
    /// `offer` is the new `wl_data_offer` id, `position` its entry point.
    /// Replaces any offer from a previous, already-ended drag.
    pub fn on_enter(&mut self, offer: u32, position: (i32, i32)) {
        self.incoming_offer = Some(DataOfferId::from_raw(offer));
        self.incoming_mime_types.clear();
        self.incoming_position = position;
    }

    /// Handle a `wl_data_device.motion` event: track where over our surface
    /// the drag currently is, for [`Self::handle_drop`] to report and for a
    /// window to paint a drop-target highlight that follows the pointer.
    pub const fn on_motion(&mut self, position: (i32, i32)) {
        self.incoming_position = position;
    }

    /// The drag's current surface-local position, if one is over us.
    #[must_use]
    pub const fn incoming_position(&self) -> (i32, i32) {
        self.incoming_position
    }

    /// Handle a `wl_data_device.leave` event: the drag left our surface (or
    /// was cancelled) before a drop. Drops the offer and its MIME list.
    pub fn on_leave(&mut self) {
        self.incoming_offer = None;
        self.incoming_mime_types.clear();
    }

    /// The incoming offer's id, if a drag is currently over us -- for the
    /// window's dispatch to route `wl_data_offer.*` events here.
    #[must_use]
    pub const fn incoming_offer_id(&self) -> Option<DataOfferId> {
        self.incoming_offer
    }

    /// Whether a drag is currently hovering our surface (an offer has
    /// arrived and not yet left or dropped) -- for painting a drop-target
    /// highlight.
    #[must_use]
    pub const fn is_drag_over(&self) -> bool {
        self.incoming_offer.is_some()
    }

    /// A `wl_data_offer.offer` event: the peer advertises one MIME type.
    pub fn on_offer_mime_type(&mut self, mime_type: String) {
        self.incoming_mime_types.push(mime_type);
    }

    /// Whether the current incoming offer advertised `text/uri-list` -- the
    /// type this controller knows how to consume. A window can use this to
    /// paint a "cannot accept" cursor for a drag carrying only, say, plain
    /// text.
    #[must_use]
    pub fn incoming_offers_files(&self) -> bool {
        self.incoming_mime_types.iter().any(|m| m == URI_LIST_MIME)
    }

    /// A `wl_data_offer.offer` event for the incoming offer (only the MIME
    /// type this controller cares about; `source_actions`/`action` are v3
    /// negotiation we don't use, plain copy being our only supported
    /// action).
    pub fn on_offer_event(&mut self, event: DataOfferEvent, mime_type: Option<String>) {
        if event == DataOfferEvent::Offer
            && let Some(m) = mime_type
        {
            self.on_offer_mime_type(m);
        }
    }

    /// Handle a `wl_data_device.drop` event: ask the offer to transfer its
    /// `text/uri-list` payload over a pipe, and spawn a helper thread to
    /// read it (see module docs for why not synchronously). Once the read
    /// completes, `wake` is signalled so the UI loop notices without
    /// polling, exactly as [`crate::clipboard::ClipboardState::request_paste`]
    /// does for a paste.
    ///
    /// No-op without a live offer or if it never advertised any files.
    /// Clears the offer immediately either way -- a drop is terminal
    /// regardless of whether a payload is coming.
    pub fn handle_drop(&mut self, window_tx: &WindowSender, wake: &waker::Waker) {
        let Some(offer) = self.incoming_offer.take() else {
            return;
        };
        let offers_files = self.incoming_offers_files();
        self.incoming_mime_types.clear();
        if !offers_files {
            return;
        }
        let Ok((mut reader, writer)) = std::os::unix::net::UnixStream::pair() else {
            return;
        };
        window_tx.send_with_fds(
            MessageBuilder::new(offer, DataOfferRequest::Receive)
                .string(URI_LIST_MIME)
                .finish(),
            vec![writer.into()],
        );
        let tx = self.drop_tx.clone();
        let wake = wake.clone();
        let position = self.incoming_position;
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            if reader.read_to_end(&mut buf).is_ok() {
                let body = String::from_utf8_lossy(&buf);
                let paths = crate::file_uri::UriList::decode(&body);
                if !paths.is_empty() {
                    let _ = tx.send(Dropped { paths, position });
                    wake.signal();
                }
            }
        });
    }

    /// Drain any completed drop, for the caller to act on (move/copy files
    /// into the directory that was the drop target). At most one is ever
    /// pending at a time -- a drop is a single discrete gesture, not a
    /// stream -- but this drains in a loop like
    /// [`crate::clipboard::ClipboardState::drain_pastes`] for the same
    /// reason: a stale, unread completion must never silently accumulate.
    pub fn drain_drops(&self, mut push: impl FnMut(Dropped)) {
        while let Ok(dropped) = self.drop_rx.try_recv() {
            push(dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn without_a_manager_every_method_is_a_harmless_no_op() {
        let mut drag = DragController::new(None);
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        assert!(!drag.is_dragging());
        assert_eq!(drag.device_id(), None);
    }

    #[test]
    fn starting_a_drag_with_no_paths_is_a_no_op() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(&mut window_tx, SurfaceId(1), 7, &[]);
        assert!(!drag.is_dragging());
    }

    #[test]
    fn starting_a_drag_creates_a_source() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        assert!(drag.is_dragging());
        assert!(drag.source_id().is_some());
    }

    #[test]
    fn a_second_start_drag_while_one_is_live_is_ignored() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        let first = drag.source_id();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            8,
            &[std::path::PathBuf::from("/b.txt")],
        );
        assert_eq!(drag.source_id(), first, "must not replace a live drag");
    }

    #[test]
    fn send_arms_the_awaiting_fd_flag() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        assert!(!drag.is_awaiting_send_fd());
        drag.on_source_event(DataSourceEvent::Send);
        assert!(drag.is_awaiting_send_fd());
    }

    #[test]
    fn cancelled_ends_the_drag() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        drag.on_source_event(DataSourceEvent::Cancelled);
        assert!(!drag.is_dragging());
    }

    #[test]
    fn dnd_finished_ends_the_drag() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        drag.on_source_event(DataSourceEvent::DndFinished);
        assert!(!drag.is_dragging());
    }

    #[test]
    fn an_incoming_enter_is_tracked_as_a_drag_over() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        assert!(!drag.is_drag_over());
        drag.on_enter(42, (0, 0));
        assert!(drag.is_drag_over());
        assert_eq!(drag.incoming_offer_id(), Some(DataOfferId(42)));
    }

    #[test]
    fn leave_clears_the_incoming_offer() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        drag.on_enter(42, (0, 0));
        drag.on_leave();
        assert!(!drag.is_drag_over());
        assert_eq!(drag.incoming_offer_id(), None);
    }

    #[test]
    fn a_fresh_offer_advertises_no_mime_types_yet() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        drag.on_enter(1, (0, 0));
        assert!(!drag.incoming_offers_files());
    }

    #[test]
    fn offering_uri_list_is_detected() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        drag.on_enter(1, (0, 0));
        drag.on_offer_mime_type("text/plain".to_string());
        assert!(!drag.incoming_offers_files());
        drag.on_offer_mime_type(URI_LIST_MIME.to_string());
        assert!(drag.incoming_offers_files());
    }

    #[test]
    fn a_new_enter_clears_the_previous_drags_mime_types() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        drag.on_enter(1, (0, 0));
        drag.on_offer_mime_type(URI_LIST_MIME.to_string());
        assert!(drag.incoming_offers_files());
        drag.on_enter(2, (0, 0));
        assert!(
            !drag.incoming_offers_files(),
            "a fresh drag must not inherit the previous one's types"
        );
    }

    #[test]
    fn dropping_without_advertised_files_reports_nothing() {
        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        drag.on_enter(1, (0, 0));
        drag.on_offer_mime_type("text/plain".to_string());
        drag.handle_drop(&window_tx, &wake);
        assert!(!drag.is_drag_over(), "the offer must still be cleared");
        let mut got = Vec::new();
        drag.drain_drops(|d| got.push(d));
        assert!(got.is_empty());
    }

    #[test]
    fn a_drop_with_no_data_device_is_harmless() {
        let mut drag = DragController::new(None);
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        drag.on_enter(1, (0, 0));
        drag.on_offer_mime_type(URI_LIST_MIME.to_string());
        drag.handle_drop(&window_tx, &wake);
        assert!(!drag.is_drag_over());
    }

    #[test]
    fn draining_drops_with_nothing_pending_yields_nothing() {
        let drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let mut got = Vec::new();
        drag.drain_drops(|d| got.push(d));
        assert!(got.is_empty());
    }

    /// End-to-end through a real pipe: write a `text/uri-list` body to the
    /// write end `handle_drop` handed out (standing in for the peer),
    /// and confirm the helper thread decodes it and delivers it through
    /// `drain_drops`.
    #[test]
    fn a_real_write_on_the_pipe_is_decoded_and_delivered() {
        use std::io::Write as _;

        let mut drag = DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))));
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        drag.on_enter(1, (0, 0));
        drag.on_offer_mime_type(URI_LIST_MIME.to_string());

        // `handle_drop` sends the write end as an ancillary fd on the
        // (unexecuted, in this test) `Receive` request; recover a peer-side
        // pipe of our own to drive the same scenario deterministically,
        // since there is no real compositor here to hand the fd onward.
        let (mut peer_writer, drop_reader) = std::os::unix::net::UnixStream::pair().expect("pipe");
        let tx = drag.drop_tx.clone();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut reader = drop_reader;
            let mut buf = Vec::new();
            if reader.read_to_end(&mut buf).is_ok() {
                let body = String::from_utf8_lossy(&buf);
                let paths = crate::file_uri::UriList::decode(&body);
                if !paths.is_empty() {
                    let _ = tx.send(Dropped {
                        paths,
                        position: (0, 0),
                    });
                }
            }
        });
        peer_writer.write_all(b"file:///a.txt\r\n").expect("write");
        drop(peer_writer);

        // Poll briefly for the helper thread to finish -- no compositor
        // round-trip involved, so this settles in well under the timeout.
        let mut got = Vec::new();
        for _ in 0..200 {
            drag.drain_drops(|d| got.push(d));
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            got,
            vec![Dropped {
                paths: vec![std::path::PathBuf::from("/a.txt")],
                position: (0, 0),
            }]
        );
        let _ = window_tx;
        let _ = wake;
    }

    #[test]
    fn without_a_manager_no_offer_can_ever_be_pending() {
        let drag = DragController::new(None);
        assert_eq!(drag.device_id(), None);
        assert!(!drag.is_drag_over());
    }
}
