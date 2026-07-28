//! The queue pairing `data_source.send` requests with their pipe descriptors.
//!
//! Wayland splits one clipboard request across two deliveries: the `send`
//! message names a MIME type, and the pipe to write it into arrives *out of
//! band* as ancillary data. Nothing in either half identifies the other -- the
//! only correlation is **arrival order**.
//!
//! That is fine until a peer requests several representations at once, which
//! is the normal case rather than the exotic one: a browser handed a
//! screenshot probes `image/png` and `text/uri-list` together, so both `send`
//! messages and both pipes can land in a single receive batch. A transport
//! that remembers only the *latest* pending MIME then writes the wrong
//! payload into the wrong pipe and closes the other one unanswered -- the peer
//! reads zero bytes and shows an empty image.
//!
//! Hence a FIFO rather than a slot: it is the same ordering guarantee the
//! transport already relies on, just written down where it can be tested
//! without a compositor.

use std::collections::VecDeque;

/// MIME types whose `send` has been seen but whose pipe has not yet arrived.
///
/// Deliberately holds no descriptors: this is the ordering *policy*, so it
/// stays in this Wayland-free crate and every case below is an ordinary unit
/// test. The transport owns the fds and asks this queue who they belong to.
#[derive(Debug, Default)]
pub struct PendingSends {
    /// Requests in arrival order; the front is the oldest unanswered `send`.
    queue: VecDeque<String>,
}

impl PendingSends {
    /// An empty queue.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    /// Record a `send` request for `mime`, whose pipe has not arrived yet.
    ///
    /// Duplicates are kept rather than deduplicated: a peer may legitimately
    /// request the same representation twice, and each request gets its own
    /// pipe that must be answered and closed independently.
    pub fn push(&mut self, mime: impl Into<String>) {
        self.queue.push_back(mime.into());
    }

    /// Claim the oldest unanswered request, for the pipe that just arrived.
    ///
    /// `None` means a descriptor arrived with no request outstanding -- an fd
    /// belonging to some other protocol (a keymap, say), which the caller must
    /// not write clipboard data into.
    pub fn claim(&mut self) -> Option<String> {
        self.queue.pop_front()
    }

    /// Whether any request is still awaiting its pipe.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        !self.queue.is_empty()
    }

    /// How many requests are outstanding.
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether nothing is outstanding.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::PendingSends;

    /// The regression this type exists for. Two `send` events arriving before
    /// either pipe -- which is what a browser probing both representations
    /// produces -- must be answered in order. A single-slot predecessor
    /// overwrote the first MIME with the second, wrote the second payload into
    /// the *first* pipe, and left the second pipe closed and empty: a
    /// screenshot that pastes as a black box.
    #[test]
    fn two_requests_before_any_pipe_are_answered_in_arrival_order() {
        let mut pending = PendingSends::new();
        pending.push("text/uri-list");
        pending.push("image/png");
        assert_eq!(pending.claim().as_deref(), Some("text/uri-list"));
        assert_eq!(pending.claim().as_deref(), Some("image/png"));
        assert_eq!(pending.claim(), None);
    }

    #[test]
    fn interleaved_request_and_pipe_still_pairs_correctly() {
        // The easy case a single slot also handled; it must keep working.
        let mut pending = PendingSends::new();
        pending.push("image/png");
        assert_eq!(pending.claim().as_deref(), Some("image/png"));
        pending.push("text/uri-list");
        assert_eq!(pending.claim().as_deref(), Some("text/uri-list"));
    }

    #[test]
    fn a_descriptor_with_no_outstanding_request_is_not_claimed() {
        // Writing clipboard bytes into an unrelated fd (a keymap, a paste pipe
        // from another object) would corrupt whatever that fd really is.
        let mut pending = PendingSends::new();
        assert_eq!(pending.claim(), None);
        assert!(!pending.is_waiting());
    }

    #[test]
    fn the_same_mime_requested_twice_gets_two_answers() {
        // Each request owns its own pipe; collapsing them leaves one peer
        // hanging on a descriptor nobody ever writes to or closes.
        let mut pending = PendingSends::new();
        pending.push("image/png");
        pending.push("image/png");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending.claim().as_deref(), Some("image/png"));
        assert_eq!(pending.claim().as_deref(), Some("image/png"));
        assert!(pending.is_empty());
    }

    #[test]
    fn waiting_reflects_outstanding_requests() {
        let mut pending = PendingSends::new();
        assert!(pending.is_empty());
        pending.push("image/png");
        assert!(pending.is_waiting());
        pending.claim();
        assert!(!pending.is_waiting());
    }
}
