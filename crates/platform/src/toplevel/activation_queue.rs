//! Collecting activation tokens as the compositor answers, without waiting.
//!
//! A token request is sent by [`crate::toplevel::activation::Activation::begin`]
//! and answered later by a `done` event on the token object. The window
//! routes every event it receives through [`ActivationQueue::absorb`]; an
//! answer becomes an [`ActivationOutcome`] the owner collects with
//! [`ActivationQueue::pop`].
//!
//! A request that can never be answered -- no `xdg_activation_v1` global, or
//! no input serial yet -- is resolved at once with no token, so the owner
//! handles every launch the same way: hold it against its ticket, spawn it
//! when the outcome arrives.

use std::collections::VecDeque;

use crate::event::RawEvent;
use crate::id::{ActivationTokenId, ObjectId};
use crate::protocol::ActivationTokenEvent;
use crate::toplevel::activation::ActivationToken;
use crate::wire::WireEvent;

/// Names one token request, so its answer can be matched to the launch that
/// asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActivationTicket(u64);

/// The answer to one token request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationOutcome {
    /// The request this answers.
    pub ticket: ActivationTicket,
    /// The token, or `None` when the launch should proceed without one.
    pub token: Option<ActivationToken>,
}

/// Token requests in flight, and answers not yet collected.
#[derive(Debug, Default)]
pub struct ActivationQueue {
    next_ticket: u64,
    in_flight: Vec<(ActivationTokenId, ActivationTicket)>,
    ready: VecDeque<ActivationOutcome>,
}

impl ActivationQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a request was sent on `object`, and name it.
    pub fn track(&mut self, object: ActivationTokenId) -> ActivationTicket {
        let ticket = self.issue();
        self.in_flight.push((object, ticket));
        ticket
    }

    /// Name a request that will never be sent, and resolve it with no token.
    pub fn resolve_unavailable(&mut self) -> ActivationTicket {
        let ticket = self.issue();
        self.ready.push_back(ActivationOutcome {
            ticket,
            token: None,
        });
        ticket
    }

    /// Take `event` if it is the answer to a tracked request.
    ///
    /// Returns the token object so the caller can destroy it; `None` means
    /// the event is not ours and must be routed on.
    pub fn absorb(&mut self, event: &RawEvent) -> Option<ActivationTokenId> {
        let RawEvent::Wire {
            object_id,
            opcode,
            bytes,
            ..
        } = event
        else {
            return None;
        };
        if ActivationTokenEvent::from_opcode(*opcode) != Some(ActivationTokenEvent::Done) {
            return None;
        }
        let position = self
            .in_flight
            .iter()
            .position(|(object, _)| object.raw() == *object_id)?;
        let (object, ticket) = self.in_flight.remove(position);
        let token = WireEvent::parse(bytes)
            .map(|(mut wire, _)| ActivationToken::new(wire.string()))
            .filter(|token| !token.as_str().is_empty());
        self.ready.push_back(ActivationOutcome { ticket, token });
        Some(object)
    }

    /// The oldest answer not yet collected.
    pub fn pop(&mut self) -> Option<ActivationOutcome> {
        self.ready.pop_front()
    }

    const fn issue(&mut self) -> ActivationTicket {
        let ticket = ActivationTicket(self.next_ticket);
        self.next_ticket += 1;
        ticket
    }
}

/// Launches waiting for their token.
///
/// Generic over what a launch is -- a URI, a path -- so each program holds
/// its own payload against the ticket and gets it back with the outcome.
#[derive(Debug)]
pub struct PendingLaunches<T> {
    entries: Vec<(ActivationTicket, T)>,
}

impl<T> Default for PendingLaunches<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<T> PendingLaunches<T> {
    /// Nothing waiting.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Hold `launch` until `ticket` is answered.
    pub fn hold(&mut self, ticket: ActivationTicket, launch: T) {
        self.entries.push((ticket, launch));
    }

    /// Release the launch waiting on `ticket`, if one is.
    pub fn take(&mut self, ticket: ActivationTicket) -> Option<T> {
        let position = self.entries.iter().position(|(held, _)| *held == ticket)?;
        Some(self.entries.remove(position).1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ActivationTokenRequest;
    use crate::wire::MessageBuilder;

    /// `done` is opcode 0, which is `SetSerial`'s number on the request
    /// side -- the builder writes whichever opcode it is handed, and the two
    /// enums are separate namespaces on the wire.
    fn done(token: ActivationTokenId, token_string: &str) -> RawEvent {
        RawEvent::from_wire_msg(
            &MessageBuilder::new(token, ActivationTokenRequest::SetSerial)
                .string(token_string)
                .finish(),
        )
    }

    #[test]
    fn an_answer_resolves_the_ticket_that_asked() {
        let mut queue = ActivationQueue::new();
        let ticket = queue.track(ActivationTokenId(15));

        let released = queue.absorb(&done(ActivationTokenId(15), "tok-xyz"));

        assert_eq!(released, Some(ActivationTokenId(15)));
        let outcome = queue.pop().expect("an outcome");
        assert_eq!(outcome.ticket, ticket);
        assert_eq!(
            outcome.token.as_ref().map(ActivationToken::as_str),
            Some("tok-xyz")
        );
    }

    #[test]
    fn answers_out_of_order_still_match_their_tickets() {
        let mut queue = ActivationQueue::new();
        let first = queue.track(ActivationTokenId(15));
        let second = queue.track(ActivationTokenId(16));

        queue.absorb(&done(ActivationTokenId(16), "two"));
        queue.absorb(&done(ActivationTokenId(15), "one"));

        let a = queue.pop().expect("first answer");
        let b = queue.pop().expect("second answer");
        assert_eq!((a.ticket, b.ticket), (second, first));
    }

    #[test]
    fn an_event_for_another_object_is_not_taken() {
        let mut queue = ActivationQueue::new();
        queue.track(ActivationTokenId(15));

        assert_eq!(queue.absorb(&done(ActivationTokenId(99), "x")), None);
        assert!(queue.pop().is_none());
    }

    #[test]
    fn a_request_that_cannot_be_sent_resolves_with_no_token() {
        let mut queue = ActivationQueue::new();
        let ticket = queue.resolve_unavailable();

        assert_eq!(
            queue.pop(),
            Some(ActivationOutcome {
                ticket,
                token: None
            })
        );
    }

    #[test]
    fn a_pending_launch_comes_back_with_its_own_ticket() {
        let mut queue = ActivationQueue::new();
        let first = queue.resolve_unavailable();
        let second = queue.resolve_unavailable();
        let mut pending = PendingLaunches::new();
        pending.hold(first, "a");
        pending.hold(second, "b");

        assert_eq!(pending.take(second), Some("b"));
        assert_eq!(pending.take(second), None);
        assert_eq!(pending.take(first), Some("a"));
    }
}
