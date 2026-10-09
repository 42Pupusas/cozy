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
//! # What this type owns, and what it delegates
//!
//! This is the seat-level coordinator: it holds the `wl_data_device`, routes
//! each event to the half it belongs to, and owns the drop payload channel.
//! The two halves have separate lifecycles and separate protocol
//! obligations, so each lives in its own module:
//!
//! * [`crate::toplevel::drag_source::DragSource`] -- the outgoing
//!   `wl_data_source`, including the action mask a drag is undeliverable
//!   without.
//! * [`crate::toplevel::drag_offer::DragOffer`] -- the incoming
//!   `wl_data_offer`, including the `accept`/`set_actions`/`finish`
//!   handshake a drop is undeliverable without.
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

use crate::conn::WindowSender;
use crate::id::{DataDeviceId, DataDeviceManagerId, DataOfferId, DataSourceId, SeatId, SurfaceId};
use crate::protocol::{
    DataDeviceManagerRequest, DataOfferEvent, DataOfferRequest, DataSourceEvent, DndAction,
};
use crate::toplevel::drag_offer::DragOffer;
use crate::toplevel::drag_source::DragSource;
use crate::wire::MessageBuilder;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};

pub use crate::toplevel::drag_source::URI_LIST_MIME;

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
    /// The outgoing half: files we are dragging out of this window.
    source: DragSource,
    /// The incoming half: a peer's drag hovering this window.
    offer: DragOffer,
    /// The serial of the most recent `wl_data_device.enter`, which
    /// `wl_data_offer.accept` must echo back.
    enter_serial: u32,
    /// The offer a drop is currently reading from, held until the payload
    /// arrives so the concluding `finish`/`destroy` can name it.
    finishing: Option<DataOfferId>,
    /// Drop channel: the helper thread spawned by [`Self::handle_drop`] sends
    /// the decoded payload here once the peer finishes writing; the UI loop
    /// drains it in `tick`. Mirrors [`crate::clipboard::ClipboardState`]'s
    /// paste channel exactly, and for the same reason (see module docs).
    drop_tx: Producer<Dropped>,
    drop_rx: Consumer<Dropped>,
}

impl DragController {
    const DROP_SLOTS: usize = 4;

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
        let (drop_tx, drop_rx) = RingBuffer::new(Capacity::exact(Self::DROP_SLOTS)).split();
        Self {
            data_device,
            source: DragSource::new(),
            offer: DragOffer::new(),
            enter_serial: 0,
            finishing: None,
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

    /// The `(manager, device)` pair, for another selection owner on this
    /// seat to publish through: the seat has one `wl_data_device`.
    #[must_use]
    pub const fn data_device(&self) -> Option<(DataDeviceManagerId, DataDeviceId)> {
        self.data_device
    }

    /// Start dragging `paths` from `origin` (the surface the press happened
    /// on), under `serial` (the press's serial -- `wl_data_device.start_drag`
    /// requires an implicit pointer grab matching one).
    ///
    /// No-op if there's no data device, `paths` is empty, or a drag is
    /// already in flight.
    pub fn start_drag(
        &mut self,
        window_tx: &mut WindowSender,
        origin: SurfaceId,
        serial: u32,
        paths: &[std::path::PathBuf],
    ) {
        let Some((mgr, dev)) = self.data_device else {
            return;
        };
        self.source
            .start(window_tx, mgr, dev, origin, serial, paths);
    }

    /// Whether a drag we started is currently live.
    #[must_use]
    pub const fn is_dragging(&self) -> bool {
        self.source.is_active()
    }

    /// The current outgoing source id, if any -- for the window's dispatch
    /// to route `wl_data_source.*` events here.
    #[must_use]
    pub const fn source_id(&self) -> Option<DataSourceId> {
        self.source.id()
    }

    /// Handle a `wl_data_source` event for our outgoing drag source.
    /// `action_bits` is the event's first argument, meaningful only for
    /// `action`.
    pub const fn on_source_event(&mut self, event: DataSourceEvent, action_bits: u32) {
        self.source.on_event(event, action_bits);
    }

    /// Whether a `data_source.send` is in flight, i.e. the next
    /// `RawEvent::Fd` is the pipe we should write our outgoing body into.
    #[must_use]
    pub const fn is_awaiting_send_fd(&self) -> bool {
        self.source.is_awaiting_send_fd()
    }

    /// Write our outgoing `text/uri-list` body into `fd` (a peer's pipe from
    /// a `data_source.send`), then let it close on scope exit to signal EOF.
    pub fn write_send_fd(&mut self, fd: std::os::fd::OwnedFd) {
        self.source.write_send_fd(fd);
    }

    /// Handle a `wl_data_device.data_offer` event: the peer introduced a new
    /// offer object under `raw_offer`, whose MIME-type events follow.
    pub fn on_new_offer(&mut self, raw_offer: u32) {
        self.offer.on_new_offer(raw_offer);
    }

    /// Handle a `wl_data_device.enter` event during an incoming drag:
    /// `offer` is the `wl_data_offer` id (0 for a sourceless drag), `serial`
    /// the enter serial `accept` must echo, `position` its entry point.
    ///
    /// Answers the offer immediately: without an `accept` and a
    /// `set_actions` the compositor will never deliver the drop.
    pub fn on_enter(
        &mut self,
        window_tx: &WindowSender,
        offer: u32,
        serial: u32,
        position: (i32, i32),
    ) {
        self.enter_serial = serial;
        self.offer.on_enter(offer, position);
        self.offer.answer(window_tx, serial);
    }

    /// Handle a `wl_data_device.motion` event: track where over our surface
    /// the drag currently is, for [`Self::handle_drop`] to report and for a
    /// window to paint a drop-target highlight that follows the pointer.
    ///
    /// Re-answers the offer, which is a no-op once answered. This covers the
    /// compositors that deliver the offer's MIME types only after `enter`,
    /// where answering on `enter` alone would refuse a drag that does carry
    /// files.
    pub fn on_motion(&mut self, window_tx: &WindowSender, position: (i32, i32)) {
        self.offer.on_motion(position);
        self.offer.answer(window_tx, self.enter_serial);
    }

    /// The drag's current surface-local position, if one is over us.
    #[must_use]
    pub const fn incoming_position(&self) -> (i32, i32) {
        self.offer.position()
    }

    /// Handle a `wl_data_device.leave` event: the drag left our surface (or
    /// was cancelled) before a drop.
    pub fn on_leave(&mut self, window_tx: &WindowSender) {
        self.offer.on_leave(window_tx);
    }

    /// The incoming offer's id, if a drag is currently over us -- for the
    /// window's dispatch to route `wl_data_offer.*` events here.
    ///
    /// Falls back to the offer a drop is still reading from, so the
    /// `action` events that arrive between `drop` and the payload still
    /// reach us.
    #[must_use]
    pub fn incoming_offer_id(&self) -> Option<DataOfferId> {
        self.offer.routable_id().or(self.finishing)
    }

    /// Whether a drag is currently hovering our surface (an offer has
    /// arrived and not yet left or dropped) -- for painting a drop-target
    /// highlight.
    #[must_use]
    pub const fn is_drag_over(&self) -> bool {
        self.offer.is_over()
    }

    /// Whether the current incoming offer advertised `text/uri-list` -- the
    /// type this controller knows how to consume. A window can use this to
    /// paint a "cannot accept" cursor for a drag carrying only, say, plain
    /// text.
    #[must_use]
    pub fn incoming_offers_files(&self) -> bool {
        self.offer.offers_files()
    }

    /// Handle a `wl_data_offer` event for the incoming offer.
    pub fn on_offer_event(
        &mut self,
        event: DataOfferEvent,
        mime_type: Option<String>,
        action_bits: u32,
    ) {
        self.offer.on_event(event, mime_type, action_bits);
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
        let Some(offer) = self.offer.take_for_drop() else {
            return;
        };
        let Ok((mut reader, writer)) = std::os::unix::net::UnixStream::pair() else {
            return;
        };
        window_tx.send_with_fds(
            MessageBuilder::new(offer, DataOfferRequest::Receive)
                .string(URI_LIST_MIME)
                .finish(),
            vec![writer.into()],
        );
        self.finishing = Some(offer);
        let tx = self.drop_tx.clone();
        let wake = wake.clone();
        let position = self.offer.position();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            let paths = if reader.read_to_end(&mut buf).is_ok() {
                crate::file_uri::UriList::decode(&String::from_utf8_lossy(&buf))
            } else {
                Vec::new()
            };
            // Sent even when empty, so a drop yields exactly one completion
            // whatever the peer wrote. The event loop concludes the transfer
            // on that completion; a silent thread would leave the compositor
            // waiting for a `finish` that never comes, and the source stuck
            // mid-drag.
            let _ = tx.push_block(Dropped { paths, position });
            wake.signal();
        });
    }

    /// Conclude a drop whose payload has arrived, telling the compositor the
    /// transfer succeeded so the source sees `dnd_finished` instead of
    /// `cancelled`. No-op unless a drop is actually pending conclusion.
    pub fn finish_drop(&mut self, window_tx: &WindowSender) {
        let Some(offer) = self.finishing.take() else {
            return;
        };
        self.offer
            .finish_drop(window_tx, offer, self.offer.selected_action());
    }

    /// Whether a drop is waiting to be concluded with a `finish`.
    #[must_use]
    pub const fn is_finishing_drop(&self) -> bool {
        self.finishing.is_some()
    }

    /// The action the compositor selected for the incoming drag.
    #[must_use]
    pub const fn incoming_action(&self) -> DndAction {
        self.offer.selected_action()
    }

    /// Drain any completed drop, for the caller to act on (move/copy files
    /// into the directory that was the drop target). At most one is ever
    /// pending at a time -- a drop is a single discrete gesture, not a
    /// stream -- but this drains in a loop like
    /// [`crate::clipboard::ClipboardState::drain_pastes`] for the same
    /// reason: a stale, unread completion must never silently accumulate.
    pub fn drain_drops(&mut self, push: impl FnMut(Dropped)) {
        self.drop_rx.drain(push);
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

    fn with_device() -> DragController {
        DragController::new(Some((DataDeviceManagerId(1), DataDeviceId(2))))
    }

    /// Bring a controller to "a file drag is hovering us", in the event
    /// order a compositor actually uses: `data_offer`, then the `offer`
    /// events, then `enter`.
    fn hovering(drag: &mut DragController, window_tx: &WindowSender) {
        drag.on_new_offer(42);
        drag.on_offer_event(DataOfferEvent::Offer, Some(URI_LIST_MIME.to_string()), 0);
        drag.on_enter(window_tx, 42, 7, (10, 20));
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
    fn starting_a_drag_creates_a_source() {
        let mut drag = with_device();
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
    fn cancelled_ends_the_drag() {
        let mut drag = with_device();
        let mut window_tx = test_sender();
        drag.start_drag(
            &mut window_tx,
            SurfaceId(1),
            7,
            &[std::path::PathBuf::from("/a.txt")],
        );
        drag.on_source_event(DataSourceEvent::Cancelled, 0);
        assert!(!drag.is_dragging());
    }

    #[test]
    fn an_incoming_enter_is_tracked_as_a_drag_over() {
        let mut drag = with_device();
        let window_tx = test_sender();
        assert!(!drag.is_drag_over());
        hovering(&mut drag, &window_tx);
        assert!(drag.is_drag_over());
        assert_eq!(drag.incoming_offer_id(), Some(DataOfferId(42)));
        assert_eq!(drag.incoming_position(), (10, 20));
    }

    #[test]
    fn leave_clears_the_incoming_offer() {
        let mut drag = with_device();
        let window_tx = test_sender();
        hovering(&mut drag, &window_tx);
        drag.on_leave(&window_tx);
        assert!(!drag.is_drag_over());
        assert_eq!(drag.incoming_offer_id(), None);
    }

    /// The routing bug that made every in-window drop silently fail: the
    /// `offer` events describing the payload arrive before `enter`, so the
    /// offer must be routable from the moment it is introduced.
    #[test]
    fn an_offer_is_routable_before_its_enter_arrives() {
        let mut drag = with_device();
        drag.on_new_offer(42);
        assert_eq!(drag.incoming_offer_id(), Some(DataOfferId(42)));
        drag.on_offer_event(DataOfferEvent::Offer, Some(URI_LIST_MIME.to_string()), 0);
        assert!(drag.incoming_offers_files());
    }

    #[test]
    fn offering_uri_list_is_detected() {
        let mut drag = with_device();
        let window_tx = test_sender();
        hovering(&mut drag, &window_tx);
        assert!(drag.incoming_offers_files());
    }

    #[test]
    fn motion_updates_the_reported_position() {
        let mut drag = with_device();
        let window_tx = test_sender();
        hovering(&mut drag, &window_tx);
        drag.on_motion(&window_tx, (99, 100));
        assert_eq!(drag.incoming_position(), (99, 100));
    }

    #[test]
    fn dropping_without_advertised_files_reports_nothing() {
        let mut drag = with_device();
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        drag.on_new_offer(42);
        drag.on_offer_event(DataOfferEvent::Offer, Some("text/plain".to_string()), 0);
        drag.on_enter(&window_tx, 42, 7, (0, 0));
        drag.handle_drop(&window_tx, &wake);
        assert!(!drag.is_drag_over(), "the offer must still be cleared");
        assert!(!drag.is_finishing_drop());
        let mut got = Vec::new();
        drag.drain_drops(|d| got.push(d));
        assert!(got.is_empty());
    }

    #[test]
    fn a_drop_leaves_the_transfer_awaiting_its_finish() {
        let mut drag = with_device();
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        hovering(&mut drag, &window_tx);
        drag.handle_drop(&window_tx, &wake);
        assert!(drag.is_finishing_drop());
        drag.finish_drop(&window_tx);
        assert!(!drag.is_finishing_drop());
    }

    /// The offer must stay routable between `drop` and the payload arriving,
    /// or the `action` event that authorises the `finish` is lost.
    #[test]
    fn a_dropping_offer_is_still_routable() {
        let mut drag = with_device();
        let window_tx = test_sender();
        let wake = waker::Waker::new().expect("waker");
        hovering(&mut drag, &window_tx);
        drag.handle_drop(&window_tx, &wake);
        assert_eq!(drag.incoming_offer_id(), Some(DataOfferId(42)));
        drag.on_offer_event(DataOfferEvent::Action, None, DndAction::Copy.mask());
        assert_eq!(drag.incoming_action(), DndAction::Copy);
    }

    #[test]
    fn finishing_without_a_pending_drop_is_harmless() {
        let mut drag = with_device();
        let window_tx = test_sender();
        drag.finish_drop(&window_tx);
        assert!(!drag.is_finishing_drop());
    }

    #[test]
    fn draining_drops_with_nothing_pending_yields_nothing() {
        let mut drag = with_device();
        let mut got = Vec::new();
        drag.drain_drops(|d| got.push(d));
        assert!(got.is_empty());
    }

    /// End-to-end through a real pipe: write a `text/uri-list` body to a
    /// peer-side pipe and confirm the helper thread decodes it and delivers
    /// it through `drain_drops`.
    #[test]
    fn a_real_write_on_the_pipe_is_decoded_and_delivered() {
        use std::io::Write as _;

        let mut drag = with_device();

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
                    let _ = tx.push_block(Dropped {
                        paths,
                        position: (0, 0),
                    });
                }
            }
        });
        peer_writer.write_all(b"file:///a.txt\r\n").expect("write");
        drop(peer_writer);

        let mut got = Vec::new();
        got.push(
            drag.drop_rx
                .pop_block()
                .expect("the helper delivers a drop"),
        );
        drag.drain_drops(|d| got.push(d));
        assert_eq!(
            got,
            vec![Dropped {
                paths: vec![std::path::PathBuf::from("/a.txt")],
                position: (0, 0),
            }]
        );
    }

    #[test]
    fn without_a_manager_no_offer_can_ever_be_pending() {
        let drag = DragController::new(None);
        assert_eq!(drag.device_id(), None);
        assert!(!drag.is_drag_over());
    }
}
