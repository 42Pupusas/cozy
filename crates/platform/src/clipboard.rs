//! Clipboard (`wl_data_device`) state for the terminal window.
//!
//! Owns everything about copy/paste over Wayland's data-device protocol: the
//! per-seat device, our outgoing `wl_data_source` (while our selection is on
//! the clipboard), the incoming `wl_data_offer` (another client's clipboard),
//! and the paste helper-thread channel. Split out of `TermWindow` so the
//! protocol bookkeeping — which is fiddly (out-of-band fds, serial rules,
//! wire arg ordering) but has nothing to do with rendering or input — lives
//! and is testable in one place.
//!
//! Deliberately free of rendering/grid concerns; the window hands it a
//! [`WindowSender`] to talk wire and reads back paste bytes / focus-serial
//! bookkeeping through its methods.

use crate::conn::WindowSender;
use crate::id::{DataDeviceId, DataDeviceManagerId, DataOfferId, DataSourceId, ObjectId, SeatId};
use crate::protocol::{
    DataDeviceManagerRequest, DataDeviceRequest, DataOfferEvent, DataOfferRequest, DataSourceEvent,
    DataSourceRequest,
};
use crate::wire::MessageBuilder;

/// Clipboard state: the data device, our outgoing source, the peer's offer,
/// and the paste channel. `None`/empty fields mean "no clipboard support" or
/// "nothing copied/offered yet" respectively — every method is a no-op in
/// those cases rather than a panic, since a compositor without
/// `wl_data_device_manager` is rare but real.
pub struct ClipboardState {
    /// `(manager, per-seat device)` if the compositor supports
    /// `wl_data_device`. `None` disables copy/paste entirely.
    data_device: Option<(DataDeviceManagerId, DataDeviceId)>,
    /// The live data source we publish while our selection is on the clipboard.
    source: Option<DataSourceId>,
    /// Text we last copied (served to other clients on `data_source.send`).
    text: String,
    /// The clipboard offer from another client (for paste), with its MIME types.
    offer: Option<DataOfferId>,
    /// Set when a `data_source.send` just arrived; the next `RawEvent::Fd` is the
    /// pipe to write our clipboard text into.
    awaiting_send_fd: bool,
    /// Paste channel: helper threads that read a paste pipe send the bytes
    /// here; the UI loop drains it in `tick`. (std mpsc — multiple paste
    /// helper threads may be in flight, so a multi-producer channel.)
    paste_tx: std::sync::mpsc::Sender<Vec<u8>>,
    paste_rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// The serial from the most recent `wl_keyboard.enter` (keyboard-focus)
    /// event. This is the serial the compositor expects in
    /// `wl_data_device.set_selection`: it proves we hold keyboard focus at
    /// the moment of the copy, and — unlike a stale `wl_pointer.button` /
    /// `wl_keyboard.key` serial from an arbitrary earlier press — it stays
    /// valid for the whole focused period. Using a stale serial is exactly
    /// what triggers the compositor's "invalid arguments" protocol error.
    focus_serial: u32,
}

impl ClipboardState {
    /// Create the per-seat data device, if the compositor advertised a
    /// `wl_data_device_manager`. Without one (rare), copy/paste is simply
    /// unavailable and every method below becomes a no-op.
    pub(crate) fn new(
        data_device_manager: Option<DataDeviceManagerId>,
        seat: SeatId,
        window_tx: &mut WindowSender,
    ) -> Self {
        let data_device = data_device_manager.map(|mgr| {
            let dev: DataDeviceId = window_tx.alloc_id();
            window_tx.request(
                MessageBuilder::new(mgr, DataDeviceManagerRequest::GetDataDevice)
                    .new_id(dev)
                    .object(seat),
            );
            (mgr, dev)
        });
        let (paste_tx, paste_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        Self {
            data_device,
            source: None,
            text: String::new(),
            offer: None,
            awaiting_send_fd: false,
            paste_tx,
            paste_rx,
            focus_serial: 0,
        }
    }

    /// The bound `wl_data_device` object id, for routing incoming events —
    /// `None` if the compositor never advertised a manager.
    pub(crate) fn device_id(&self) -> Option<DataDeviceId> {
        self.data_device.map(|(_, dev)| dev)
    }

    /// Record the latest `wl_keyboard.enter` serial. Must be called before
    /// [`Self::copy`] can publish anything (see `focus_serial`'s docs).
    pub(crate) const fn set_focus_serial(&mut self, serial: u32) {
        self.focus_serial = serial;
    }

    /// Publish `text` to the system clipboard: create a `wl_data_source`
    /// offering `text/plain` and set it as the seat's selection. No-op if
    /// `text` is empty, there's no data device, or we don't yet hold a
    /// keyboard-focus serial (copy only makes sense on a focused window, so
    /// this shouldn't happen in practice — but an invalid/stale serial is a
    /// protocol error, so we skip the publish rather than risk one).
    pub(crate) fn copy(&mut self, window_tx: &mut WindowSender, text: String) {
        if text.is_empty() {
            return;
        }
        self.text = text;
        let Some((mgr, dev)) = self.data_device else {
            return;
        };
        let serial = self.focus_serial;
        if serial == 0 {
            eprintln!("platform: copy skipped — no keyboard focus serial yet");
            return;
        }
        let src: DataSourceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(mgr, DataDeviceManagerRequest::CreateDataSource).new_id(src),
        );
        window_tx.request(MessageBuilder::new(src, DataSourceRequest::Offer).string("text/plain"));
        // Wire arg order is (source, serial): libwayland marshals args in XML
        // declaration order, and wl_data_device.set_selection declares
        // `source` before `serial`. Sending (serial, source) makes the
        // compositor read the source id as the serial and vice versa ->
        // "invalid arguments" protocol error.
        window_tx.request(
            MessageBuilder::new(dev, DataDeviceRequest::SetSelection)
                .object(src)
                .u32(serial),
        );
        self.source = Some(src);
    }

    /// Request a paste from the system clipboard: ask the current offer's
    /// source to transfer `text/plain` over a pipe. We hand the pipe's write
    /// end to the peer (out-of-band fd); it writes the clipboard contents and
    /// closes. A helper thread blocks reading our end and pushes the bytes
    /// back through the paste channel, then signals `wake` so the UI loop
    /// notices without polling. No-op if nothing is currently offered.
    pub(crate) fn request_paste(&self, window_tx: &WindowSender, wake: &waker::Waker) {
        let Some(offer) = self.offer else {
            return;
        };
        let Ok((mut reader, writer)) = std::os::unix::net::UnixStream::pair() else {
            return;
        };
        // `receive(mime_type, fd)`: the fd travels as ancillary data, not a
        // wire word, so the message body is just the mime string.
        window_tx.send_with_fds(
            MessageBuilder::new(offer, DataOfferRequest::Receive)
                .string("text/plain")
                .finish(),
            vec![writer.into()],
        );
        let tx = self.paste_tx.clone();
        let wake = wake.clone();
        std::thread::spawn(move || {
            use std::io::Read as _;
            let mut buf = Vec::new();
            if reader.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                let _ = tx.send(buf);
                wake.signal();
            }
        });
    }

    /// Drain any paste bytes a helper thread has finished reading, calling
    /// `push` once per completed paste (in arrival order).
    pub(crate) fn drain_pastes(&self, mut push: impl FnMut(Vec<u8>)) {
        while let Ok(bytes) = self.paste_rx.try_recv() {
            push(bytes);
        }
    }

    /// Whether a `data_source.send` is in flight, i.e. the next
    /// `RawEvent::Fd` is the pipe we should write our clipboard text into.
    pub(crate) const fn is_awaiting_send_fd(&self) -> bool {
        self.awaiting_send_fd
    }

    /// Write our current clipboard text into `fd` (a peer's pipe from a
    /// `data_source.send`), then let it close on scope exit to signal EOF.
    /// Clears the awaiting-fd flag. Best-effort: a write failure is silently
    /// dropped, matching the original behavior (there's no good recovery for
    /// a peer that stopped reading).
    pub(crate) fn write_send_fd(&mut self, fd: std::os::fd::OwnedFd) {
        use std::io::Write as _;
        use std::os::fd::FromRawFd as _;
        self.awaiting_send_fd = false;
        let raw = std::os::fd::IntoRawFd::into_raw_fd(fd);
        let mut f = unsafe { std::fs::File::from_raw_fd(raw) };
        let _ = f.write_all(self.text.as_bytes());
    }

    /// Handle a `wl_data_device.selection` event: `raw` is the new
    /// `wl_data_offer` id, or 0 if the selection was cleared.
    pub(crate) fn on_selection_event(&mut self, raw: u32) {
        self.offer = if raw == 0 {
            None
        } else {
            Some(DataOfferId::from_raw(raw))
        };
    }

    /// The current offer id, if any — for the window's event dispatch to
    /// route `wl_data_offer.*` events here.
    pub(crate) const fn offer_id(&self) -> Option<DataOfferId> {
        self.offer
    }

    /// The current outgoing source id, if any — for the window's event
    /// dispatch to route `wl_data_source.*` events here.
    pub(crate) const fn source_id(&self) -> Option<DataSourceId> {
        self.source
    }

    /// Handle a `wl_data_source` event for our current source: `Send` arms
    /// the awaiting-fd flag (the fd itself arrives as the next
    /// `RawEvent::Fd`); `Cancelled` drops the source (a peer replaced our
    /// selection). Other events are ignored.
    pub(crate) const fn on_source_event(&mut self, event: DataSourceEvent) {
        match event {
            DataSourceEvent::Send => self.awaiting_send_fd = true,
            DataSourceEvent::Cancelled => self.source = None,
            _ => {}
        }
    }

    /// A `wl_data_offer.offer` event arrived (a peer advertising one MIME
    /// type). We only ever request `text/plain` on paste, so there's
    /// nothing to track here beyond acknowledging the offer exists —
    /// exposed so the window's dispatch has an explicit hook to call instead
    /// of silently swallowing the event. Takes no `&self`: it's a
    /// documented no-op stub, not a real query against clipboard state.
    pub(crate) const fn on_offer_event(_event: DataOfferEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_selection_event_zero_clears_offer() {
        // Build a state with no manager (so `new`'s Wayland requests are
        // skipped) purely to exercise the pure offer-tracking logic.
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(None, SeatId(1), &mut window_tx);
        cb.on_selection_event(42);
        assert_eq!(cb.offer_id(), Some(DataOfferId(42)));
        cb.on_selection_event(0);
        assert_eq!(cb.offer_id(), None);
    }

    #[test]
    fn copy_is_noop_without_focus_serial() {
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(Some(DataDeviceManagerId(1)), SeatId(1), &mut window_tx);
        cb.copy(&mut window_tx, "hello".to_string());
        // No serial set yet -> copy must not create a source.
        assert_eq!(cb.source_id(), None);
    }

    #[test]
    fn copy_is_noop_for_empty_text() {
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(Some(DataDeviceManagerId(1)), SeatId(1), &mut window_tx);
        cb.set_focus_serial(7);
        cb.copy(&mut window_tx, String::new());
        assert_eq!(cb.source_id(), None);
    }

    #[test]
    fn copy_publishes_a_source_once_focused() {
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(Some(DataDeviceManagerId(1)), SeatId(1), &mut window_tx);
        cb.set_focus_serial(7);
        cb.copy(&mut window_tx, "hello".to_string());
        assert!(cb.source_id().is_some());
    }

    #[test]
    fn on_source_event_send_arms_awaiting_fd() {
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(None, SeatId(1), &mut window_tx);
        assert!(!cb.is_awaiting_send_fd());
        cb.on_source_event(DataSourceEvent::Send);
        assert!(cb.is_awaiting_send_fd());
    }

    #[test]
    fn on_source_event_cancelled_drops_source() {
        let mut window_tx = test_sender();
        let mut cb = ClipboardState::new(Some(DataDeviceManagerId(1)), SeatId(1), &mut window_tx);
        cb.set_focus_serial(7);
        cb.copy(&mut window_tx, "hello".to_string());
        assert!(cb.source_id().is_some());
        cb.on_source_event(DataSourceEvent::Cancelled);
        assert_eq!(cb.source_id(), None);
    }

    #[test]
    fn drain_pastes_yields_nothing_when_no_helper_thread_has_delivered() {
        let mut window_tx = test_sender();
        let cb = ClipboardState::new(None, SeatId(1), &mut window_tx);
        let mut got = Vec::new();
        cb.drain_pastes(|b| got.push(b));
        assert!(got.is_empty());
    }

    /// Build a throwaway `WindowSender` for tests that don't have a live
    /// connection: a ring big enough for the handful of requests these tests
    /// issue, backed by a fresh id block.
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
}
