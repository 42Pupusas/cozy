//! Watches the compositor's clipboard and hands back each new selection.
//!
//! This is the transport half of clipboard history: it binds a
//! `zwlr_data_control_device_v1`, follows the announce → advertise → select
//! sequence, pulls the interesting representations through pipes, and yields a
//! finished [`ClipboardPayload`]. Every policy decision (which MIME types are
//! worth taking, what counts as a duplicate, how much to retain) lives in
//! `crates/clipboard`, which needs no compositor to test.

use crate::mime_reader::MimeReader;
use clipboard::{ClipboardOffer, ClipboardPayload, IncomingClipboard, OfferRegistry};
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{
    DataControlDeviceId, DataControlManagerId, DataControlOfferId, ObjectId, RegistryId, SeatId,
};
use platform::protocol::{
    DataControlDeviceEvent, DataControlManagerRequest, DataControlOfferEvent,
    DataControlOfferRequest,
};
use platform::surface::SurfaceBootstrap;
use platform::wire::{MessageBuilder, WireEvent};
use quetzalcoatl::spsc::Consumer;

/// How many announced-but-unselected offers to track at once.
///
/// A compositor announces an offer per selection change and per primary
/// selection; a handful in flight is generous, and the bound is what keeps a
/// week-long daemon from growing without limit.
const TRACKED_OFFERS: usize = 8;

/// Follows the seat's clipboard selection and reads each new payload.
pub struct SelectionWatcher {
    device: DataControlDeviceId,
    offers: OfferRegistry,
    reader: MimeReader,
}

impl SelectionWatcher {
    /// Bind a data-control device for `seat_global` and start watching.
    ///
    /// A *fresh* seat object is bound rather than a shared one, matching the
    /// reason `shot` does the same: seat events are delivered per object, and
    /// a seat bound during registry discovery has already had its one-shot
    /// events consumed by that loop.
    pub fn bind(
        sender: &mut Sender,
        manager: DataControlManagerId,
        registry: RegistryId,
        seat_global: u32,
        seat_version: u32,
    ) -> platform::Result<Self> {
        let seat: SeatId = SurfaceBootstrap::bind(
            sender,
            registry,
            seat_global,
            platform::protocol::WL_SEAT.name,
            platform::protocol::WL_SEAT.version.min(seat_version),
        )?;
        let device: DataControlDeviceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(manager, DataControlManagerRequest::GetDevice)
                .new_id(device)
                .object(seat),
        )?;
        Ok(Self {
            device,
            offers: OfferRegistry::with_capacity(TRACKED_OFFERS),
            reader: MimeReader::new(),
        })
    }

    /// The bound device, for callers routing events themselves.
    #[must_use]
    pub const fn device(&self) -> DataControlDeviceId {
        self.device
    }

    /// Feed one event in; get a finished payload when a selection completes.
    ///
    /// Returns `None` for every event that merely advances the sequence, which
    /// is most of them: announcement and MIME advertisement carry no payload.
    pub fn handle(&mut self, sender: &mut Sender, event: &RawEvent) -> Option<ClipboardPayload> {
        if event.is(self.device) {
            return match event.event::<DataControlDeviceEvent>()? {
                DataControlDeviceEvent::DataOffer => {
                    self.offers.announce(event.arg(0));
                    None
                }
                DataControlDeviceEvent::Selection => self.on_selection(sender, event.arg(0)),
                // The primary selection (middle-click paste) is a separate
                // buffer users do not think of as "the clipboard"; recording it
                // would fill history with every stray text drag.
                DataControlDeviceEvent::PrimarySelection => {
                    self.offers.discard(event.arg(0));
                    None
                }
                DataControlDeviceEvent::Finished => None,
            };
        }
        // Otherwise it may be an `offer` event on one of the announced offer
        // objects. The id check must come *first*: opcodes are per-interface,
        // so `data_control_offer.offer` is opcode 0 -- and so is
        // `wl_callback.done`, `wl_seat.capabilities`, and much else. Matching
        // the opcode alone would try to decode an unrelated numeric event as a
        // string, which panics on a short body.
        let id = event.object_id();
        if self.offers.contains(id)
            && event.event::<DataControlOfferEvent>() == Some(DataControlOfferEvent::Offer)
        {
            let bytes = event.wire_bytes()?;
            let (mut wire, _) = WireEvent::parse(bytes)?;
            self.offers.advertise(id, wire.string());
        }
        None
    }

    /// A selection landed: read the representations worth keeping.
    ///
    /// The offer object is destroyed unconditionally afterwards. Data-control
    /// offers are single-use, and leaking one per copy is a slow resource leak
    /// in a process designed to run for weeks.
    fn on_selection(&mut self, sender: &mut Sender, raw: u32) -> Option<ClipboardPayload> {
        // A zero id means the selection was cleared. There is nothing to
        // record: history keeps what *was* copied, and an empty clipboard is
        // not a copy.
        if raw == 0 {
            return None;
        }
        let mimes = self.offers.take(raw)?;
        let offer = DataControlOfferId::from_raw(raw);

        let mut incoming = IncomingClipboard::new();
        incoming.begin_offer();
        for mime in mimes {
            incoming.advertise(mime);
        }
        let wanted = incoming.begin_receiving();

        let mut payload = ClipboardPayload::new();
        for mime in wanted {
            if let Some(bytes) = self.reader.read(sender, offer, &mime) {
                payload.offer(ClipboardOffer::new(mime, bytes));
            }
        }
        let _ = sender.request(MessageBuilder::new(offer, DataControlOfferRequest::Destroy));
        (!payload.is_empty()).then_some(payload)
    }
}

/// Drains the event ring, yielding each completed selection.
pub struct WatcherLoop;

impl WatcherLoop {
    /// Pump every currently queued event through `watcher`.
    ///
    /// Blocks for the first event, then drains without blocking, mirroring how
    /// the window loop batches a burst.
    pub fn pump(
        watcher: &mut SelectionWatcher,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        mut on_payload: impl FnMut(ClipboardPayload),
    ) -> bool {
        let Some(first) = events.pop_block() else {
            return false;
        };
        if let Some(payload) = watcher.handle(sender, &first) {
            on_payload(payload);
        }
        while let Some(event) = events.pop() {
            if let Some(payload) = watcher.handle(sender, &event) {
                on_payload(payload);
            }
        }
        true
    }
}
