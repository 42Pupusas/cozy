//! The incoming half of a drag: a peer's `wl_data_offer` while a drag
//! hovers this window.
//!
//! # The destination's side of the handshake
//!
//! A destination is not a passive observer. From `wl_data_device_manager`
//! version 3 it must answer an `enter` with two requests before the drop can
//! land:
//!
//! * `wl_data_offer.accept` -- names the MIME type it will take (or null to
//!   refuse). From v3 this decides the outcome: refuse everything and the
//!   compositor cancels the drag.
//! * `wl_data_offer.set_actions` -- declares which `dnd_action`s it supports
//!   and which it prefers. The compositor intersects this with the source's
//!   mask; an empty intersection means it withholds `wl_data_device.drop`
//!   from us entirely.
//!
//! Skipping either leaves the drag looking live -- we still get `enter`,
//! `motion` and the offered MIME list, so a drop-target highlight paints
//! correctly -- while the release silently does nothing.
//!
//! After the payload is read, `wl_data_offer.finish` tells the compositor the
//! transfer succeeded, which is what makes the source see `dnd_finished`
//! rather than `cancelled`. Sending `finish` without a selected action is a
//! protocol error, so it is gated on the negotiation having settled.

use crate::conn::WindowSender;
use crate::id::{DataOfferId, ObjectId};
use crate::protocol::{DataOfferEvent, DataOfferRequest, DndAction};
use crate::toplevel::drag_source::URI_LIST_MIME;
use crate::wire::MessageBuilder;

/// One peer's drag offer, from its `enter` until its `leave` or `drop`.
pub struct DragOffer {
    /// The offer object, while a drag is over us.
    id: Option<DataOfferId>,
    /// The offer object a `wl_data_device.data_offer` introduced but no
    /// `enter` has named yet.
    ///
    /// This is what makes the MIME list reachable at all. The `offer` events
    /// that describe the payload are sent on the offer object *before* the
    /// `enter`, so a dispatcher that routes only by [`Self::id`] discards
    /// every one of them. The list then looks empty, [`Self::answer`]
    /// refuses the drag, and the compositor cancels it -- a drop that never
    /// arrives while the hover highlight paints perfectly.
    pending: Option<DataOfferId>,
    /// MIME types this offer advertised. A drag carrying no `text/uri-list`
    /// (dragged text, say) is tracked so a window can show a "cannot accept"
    /// affordance instead of treating every hovering drag alike.
    mime_types: Vec<String>,
    /// The most recent surface-local position from `enter` or `motion`, for
    /// resolving which row a drop landed on.
    position: (i32, i32),
    /// The action the compositor selected, from `wl_data_offer.action`.
    /// [`DndAction::None`] means the negotiation has not settled, in which
    /// case no drop will be delivered.
    selected_action: DndAction,
    /// Whether we have answered this offer's `enter` yet. The answer is sent
    /// once the MIME list is known, not on `enter` itself -- the `offer`
    /// events that describe the payload arrive *before* `enter`, but a fresh
    /// offer's list is only complete by the time we act on it.
    answered: bool,
}

impl DragOffer {
    /// An idle offer: no drag over us.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            id: None,
            pending: None,
            mime_types: Vec::new(),
            position: (0, 0),
            selected_action: DndAction::None,
            answered: false,
        }
    }

    /// Handle `wl_data_device.enter`: `raw_offer` is the new offer id (0 if
    /// the drag carries no source), `position` its entry point.
    ///
    /// Replaces any offer left from an already-ended drag. Deliberately does
    /// not clear the MIME list: `wl_data_offer.offer` events arrive *before*
    /// the `enter` that names the offer, so clearing here would discard the
    /// very payload description we need. [`Self::on_new_offer`] is what
    /// starts a fresh list.
    pub fn on_enter(&mut self, raw_offer: u32, position: (i32, i32)) {
        self.id = if raw_offer == 0 {
            None
        } else {
            Some(DataOfferId::from_raw(raw_offer))
        };
        self.pending = None;
        self.position = position;
        self.selected_action = DndAction::None;
        self.answered = false;
    }

    /// Handle `wl_data_device.data_offer`: the peer introduced a brand-new
    /// offer object under `raw_offer`, whose `offer` events follow. Starts a
    /// fresh MIME list so a new drag never inherits the previous one's
    /// advertised types.
    ///
    /// The id is recorded so those `offer` events can be routed here before
    /// the `enter` -- see [`Self::pending`].
    pub fn on_new_offer(&mut self, raw_offer: u32) {
        self.mime_types.clear();
        self.pending = (raw_offer != 0).then(|| DataOfferId::from_raw(raw_offer));
    }

    /// The offer id a `wl_data_offer` event should be routed to: the entered
    /// offer, or one that was introduced but not entered yet.
    #[must_use]
    pub fn routable_id(&self) -> Option<DataOfferId> {
        self.id.or(self.pending)
    }

    /// Answer the current offer, if it has not been answered yet: accept the
    /// MIME type we can consume and declare the actions we support.
    ///
    /// Idempotent, so the caller may call it after every `enter`/`motion`
    /// without tracking whether it already ran. An offer that carries no
    /// files is explicitly *refused* (a null MIME type) rather than left
    /// unanswered, so the compositor cancels a drag we cannot use instead of
    /// leaving the user's pointer holding it.
    pub fn answer(&mut self, window_tx: &WindowSender, serial: u32) {
        let Some(offer) = self.id else {
            return;
        };
        if self.answered {
            return;
        }

        if self.offers_files() {
            self.answered = true;
            window_tx.request(
                MessageBuilder::new(offer, DataOfferRequest::Accept)
                    .u32(serial)
                    .string(URI_LIST_MIME),
            );
            window_tx.request(
                MessageBuilder::new(offer, DataOfferRequest::SetActions)
                    .u32(DndAction::Copy.mask() | DndAction::Move.mask())
                    .u32(DndAction::Copy.mask()),
            );
        } else {
            // A null mime_type is a zero-length string on the wire, which
            // `MessageBuilder::string` cannot express; the protocol encodes
            // it as a bare length word of 0.
            //
            // A refusal deliberately does not latch: a compositor that
            // delivers the MIME list late would otherwise have its drag
            // refused permanently on the strength of an empty list, with no
            // way back once the types arrive.
            window_tx.request(
                MessageBuilder::new(offer, DataOfferRequest::Accept)
                    .u32(serial)
                    .u32(0),
            );
        }
    }

    /// Handle `wl_data_device.motion`: track where over our surface the drag
    /// sits, for a drop-target highlight and for resolving the drop's target.
    pub const fn on_motion(&mut self, position: (i32, i32)) {
        self.position = position;
    }

    /// The drag's current surface-local position.
    #[must_use]
    pub const fn position(&self) -> (i32, i32) {
        self.position
    }

    /// Handle `wl_data_device.leave`: the drag left before a drop. Destroys
    /// the offer, as the protocol requires, and forgets its MIME list.
    pub fn on_leave(&mut self, window_tx: &WindowSender) {
        if let Some(offer) = self.id.take() {
            window_tx.request(MessageBuilder::new(offer, DataOfferRequest::Destroy));
        }
        self.pending = None;
        self.mime_types.clear();
        self.answered = false;
        self.selected_action = DndAction::None;
    }

    /// The offer's id, for the window's dispatch to route `wl_data_offer.*`
    /// events here.
    #[must_use]
    pub const fn id(&self) -> Option<DataOfferId> {
        self.id
    }

    /// Whether a drag is currently hovering our surface.
    #[must_use]
    pub const fn is_over(&self) -> bool {
        self.id.is_some()
    }

    /// Whether the current offer advertised `text/uri-list` -- the type we
    /// know how to consume.
    #[must_use]
    pub fn offers_files(&self) -> bool {
        self.mime_types.iter().any(|m| m == URI_LIST_MIME)
    }

    /// The action the compositor selected for this drag.
    #[must_use]
    pub const fn selected_action(&self) -> DndAction {
        self.selected_action
    }

    /// Handle one `wl_data_offer` event. `mime_type` is the string argument
    /// of an [`DataOfferEvent::Offer`], `action_bits` the first argument of
    /// an [`DataOfferEvent::Action`].
    pub fn on_event(
        &mut self,
        event: DataOfferEvent,
        mime_type: Option<String>,
        action_bits: u32,
    ) {
        match event {
            DataOfferEvent::Offer => {
                if let Some(m) = mime_type {
                    self.mime_types.push(m);
                }
            }
            DataOfferEvent::Action => {
                self.selected_action = DndAction::from_selected(action_bits);
            }
            DataOfferEvent::SourceActions => {}
        }
    }

    /// Take the offer for a drop, returning the id to request the payload
    /// from. Clears the hover state -- a drop is terminal either way.
    ///
    /// Returns `None` when there is no offer or it carries no files, in
    /// which case there is nothing to receive.
    pub fn take_for_drop(&mut self) -> Option<DataOfferId> {
        let offer = self.id.take()?;
        let offers_files = self.offers_files();
        self.pending = None;
        self.mime_types.clear();
        self.answered = false;
        offers_files.then_some(offer)
    }

    /// Conclude a drop whose payload has been read: tell the compositor the
    /// transfer succeeded, then destroy the offer.
    ///
    /// `finish` is skipped unless an action was selected -- sending it
    /// otherwise is the `invalid_finish` protocol error, which kills the
    /// connection. The destroy still happens, so the offer never leaks.
    pub fn finish_drop(&self, window_tx: &WindowSender, offer: DataOfferId, action: DndAction) {
        if action != DndAction::None {
            window_tx.request(MessageBuilder::new(offer, DataOfferRequest::Finish));
        }
        window_tx.request(MessageBuilder::new(offer, DataOfferRequest::Destroy));
    }
}

impl Default for DragOffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::WindowRequest;
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc;

    fn test_sender() -> (WindowSender, mpsc::Consumer<WindowRequest>) {
        let (tx, rx) = mpsc::RingBuffer::<WindowRequest>::new(Capacity::exact(64)).split();
        (
            WindowSender {
                tx,
                next_id: 100,
                id_end: 200,
                free_ids: Vec::new(),
            },
            rx,
        )
    }

    fn opcodes(rx: &mut mpsc::Consumer<WindowRequest>) -> Vec<u16> {
        let mut out = Vec::new();
        while let Some(WindowRequest::Send(msg)) = rx.pop() {
            out.push(u16::from_ne_bytes(msg[4..6].try_into().expect("opcode")));
        }
        out
    }

    fn hovering_with_files() -> DragOffer {
        let mut offer = DragOffer::new();
        offer.on_new_offer(42);
        offer.on_event(DataOfferEvent::Offer, Some(URI_LIST_MIME.to_string()), 0);
        offer.on_enter(42, (10, 20));
        offer
    }

    /// The bug this field exists to prevent: `offer` events precede the
    /// `enter`, so an offer with no `id` yet must still be routable, or the
    /// MIME list is never collected and every drag is refused.
    #[test]
    fn an_introduced_offer_is_routable_before_its_enter() {
        let mut offer = DragOffer::new();
        offer.on_new_offer(42);
        assert_eq!(offer.routable_id(), Some(DataOfferId(42)));
        assert!(!offer.is_over(), "introduced is not yet entered");
    }

    /// A drag refused on an empty MIME list must be reconsidered when the
    /// types arrive, rather than staying refused for the whole gesture.
    #[test]
    fn a_refusal_is_reconsidered_once_the_types_arrive() {
        let mut offer = DragOffer::new();
        offer.on_new_offer(42);
        offer.on_enter(42, (0, 0));
        let (tx, mut rx) = test_sender();
        offer.answer(&tx, 7);
        let _ = opcodes(&mut rx);

        offer.on_event(DataOfferEvent::Offer, Some(URI_LIST_MIME.to_string()), 0);
        offer.answer(&tx, 7);
        assert!(
            opcodes(&mut rx).contains(&(DataOfferRequest::SetActions as u16)),
            "a late MIME list must still be accepted"
        );
    }

    #[test]
    fn a_fresh_offer_is_not_over_us() {
        let offer = DragOffer::new();
        assert!(!offer.is_over());
        assert!(!offer.offers_files());
    }

    #[test]
    fn an_enter_is_tracked_as_a_drag_over() {
        let offer = hovering_with_files();
        assert!(offer.is_over());
        assert_eq!(offer.id(), Some(DataOfferId(42)));
        assert_eq!(offer.position(), (10, 20));
    }

    /// `wl_data_offer.offer` events precede the `enter` that names the
    /// offer, so `enter` must not discard the MIME list it has collected.
    #[test]
    fn mime_types_offered_before_enter_survive_it() {
        let offer = hovering_with_files();
        assert!(offer.offers_files());
    }

    #[test]
    fn a_new_offer_clears_the_previous_drags_mime_types() {
        let mut offer = hovering_with_files();
        assert!(offer.offers_files());
        offer.on_new_offer(43);
        assert!(
            !offer.offers_files(),
            "a fresh drag must not inherit the previous one's types"
        );
    }

    #[test]
    fn an_enter_with_no_source_leaves_us_without_an_offer() {
        let mut offer = DragOffer::new();
        offer.on_enter(0, (1, 2));
        assert!(!offer.is_over());
    }

    /// The bug this module exists to prevent: without `accept` +
    /// `set_actions` the compositor never delivers the drop.
    #[test]
    fn answering_accepts_the_type_and_declares_the_actions() {
        let mut offer = hovering_with_files();
        let (tx, mut rx) = test_sender();
        offer.answer(&tx, 7);
        let sent = opcodes(&mut rx);
        assert!(sent.contains(&(DataOfferRequest::Accept as u16)));
        assert!(sent.contains(&(DataOfferRequest::SetActions as u16)));
    }

    #[test]
    fn answering_twice_sends_nothing_the_second_time() {
        let mut offer = hovering_with_files();
        let (tx, mut rx) = test_sender();
        offer.answer(&tx, 7);
        let _ = opcodes(&mut rx);
        offer.answer(&tx, 7);
        assert!(opcodes(&mut rx).is_empty(), "answering must be idempotent");
    }

    #[test]
    fn a_drag_without_files_is_refused_rather_than_ignored() {
        let mut offer = DragOffer::new();
        offer.on_new_offer(42);
        offer.on_event(DataOfferEvent::Offer, Some("text/plain".to_string()), 0);
        offer.on_enter(42, (0, 0));
        let (tx, mut rx) = test_sender();
        offer.answer(&tx, 7);
        let sent = opcodes(&mut rx);
        assert!(sent.contains(&(DataOfferRequest::Accept as u16)));
        assert!(
            !sent.contains(&(DataOfferRequest::SetActions as u16)),
            "a refused drag must not claim to support any action"
        );
    }

    #[test]
    fn answering_without_an_offer_is_harmless() {
        let mut offer = DragOffer::new();
        let (tx, mut rx) = test_sender();
        offer.answer(&tx, 7);
        assert!(opcodes(&mut rx).is_empty());
    }

    #[test]
    fn motion_tracks_the_position() {
        let mut offer = hovering_with_files();
        offer.on_motion((99, 100));
        assert_eq!(offer.position(), (99, 100));
    }

    #[test]
    fn leaving_destroys_the_offer_and_clears_the_hover() {
        let mut offer = hovering_with_files();
        let (tx, mut rx) = test_sender();
        offer.on_leave(&tx);
        assert!(!offer.is_over());
        assert!(opcodes(&mut rx).contains(&(DataOfferRequest::Destroy as u16)));
    }

    #[test]
    fn the_compositors_chosen_action_is_recorded() {
        let mut offer = hovering_with_files();
        assert_eq!(offer.selected_action(), DndAction::None);
        offer.on_event(DataOfferEvent::Action, None, DndAction::Copy.mask());
        assert_eq!(offer.selected_action(), DndAction::Copy);
    }

    #[test]
    fn taking_for_a_drop_yields_the_offer_and_ends_the_hover() {
        let mut offer = hovering_with_files();
        assert_eq!(offer.take_for_drop(), Some(DataOfferId(42)));
        assert!(!offer.is_over());
    }

    #[test]
    fn taking_a_fileless_drop_yields_nothing_but_still_ends_the_hover() {
        let mut offer = DragOffer::new();
        offer.on_new_offer(42);
        offer.on_event(DataOfferEvent::Offer, Some("text/plain".to_string()), 0);
        offer.on_enter(42, (0, 0));
        assert_eq!(offer.take_for_drop(), None);
        assert!(!offer.is_over(), "the offer must still be cleared");
    }

    #[test]
    fn finishing_a_negotiated_drop_sends_finish_then_destroy() {
        let offer = DragOffer::new();
        let (tx, mut rx) = test_sender();
        offer.finish_drop(&tx, DataOfferId(42), DndAction::Copy);
        assert_eq!(
            opcodes(&mut rx),
            vec![
                DataOfferRequest::Finish as u16,
                DataOfferRequest::Destroy as u16
            ]
        );
    }

    /// `finish` without a selected action is the `invalid_finish` protocol
    /// error, which drops the connection.
    #[test]
    fn finishing_without_an_action_destroys_but_never_finishes() {
        let offer = DragOffer::new();
        let (tx, mut rx) = test_sender();
        offer.finish_drop(&tx, DataOfferId(42), DndAction::None);
        assert_eq!(opcodes(&mut rx), vec![DataOfferRequest::Destroy as u16]);
    }
}
