//! Pure state for receiving a compositor-owned clipboard offer.

use crate::{ClipboardOffer, ClipboardPayload};

/// Incoming offer lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncomingState {
    /// No current offer exists.
    Empty,
    /// The compositor is advertising MIME types for an offer.
    Advertising(Vec<String>),
    /// One or more MIME transfers have been requested and are being collected.
    Receiving { remaining: Vec<String> },
}

/// Chooses and assembles clipboard representations without Wayland I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingClipboard {
    state: IncomingState,
    payload: ClipboardPayload,
}

impl IncomingClipboard {
    /// Start without an offer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: IncomingState::Empty,
            payload: ClipboardPayload::new(),
        }
    }

    /// Begin tracking a newly announced offer.
    pub fn begin_offer(&mut self) {
        self.state = IncomingState::Advertising(Vec::new());
        self.payload = ClipboardPayload::new();
    }

    /// Record one MIME type advertised by the current offer.
    pub fn advertise(&mut self, mime: impl Into<String>) {
        if let IncomingState::Advertising(mimes) = &mut self.state {
            let mime = mime.into();
            if !mimes.iter().any(|known| known == &mime) {
                mimes.push(mime);
            }
        }
    }

    /// MIME types worth receiving, in preference order.
    #[must_use]
    pub fn requested_mimes(&self) -> Vec<String> {
        let IncomingState::Advertising(mimes) = &self.state else {
            return Vec::new();
        };
        let mut selected = Vec::new();
        for preferred in [
            "image/png",
            "text/uri-list",
            "text/plain;charset=utf-8",
            "text/plain",
        ] {
            if mimes.iter().any(|mime| mime == preferred) {
                selected.push(preferred.to_string());
            }
        }
        selected
    }

    /// Mark selected MIME types as requested from the compositor.
    pub fn begin_receiving(&mut self) -> Vec<String> {
        let requested = self.requested_mimes();
        self.state = IncomingState::Receiving {
            remaining: requested.clone(),
        };
        requested
    }

    /// Complete one requested transfer.
    pub fn receive(&mut self, mime: &str, bytes: Vec<u8>) {
        let IncomingState::Receiving { remaining } = &mut self.state else {
            return;
        };
        if let Some(index) = remaining.iter().position(|known| known == mime) {
            remaining.remove(index);
            self.payload.offer(ClipboardOffer::new(mime, bytes));
        }
        if remaining.is_empty() {
            self.state = IncomingState::Empty;
        }
    }

    /// The completed payload, if at least one transfer has arrived.
    #[must_use]
    pub const fn payload(&self) -> &ClipboardPayload {
        &self.payload
    }

    /// Whether all selected transfers have completed.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, IncomingState::Empty) && !self.payload.is_empty()
    }
}

impl Default for IncomingClipboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::IncomingClipboard;

    #[test]
    fn selects_rich_representations_before_text() {
        let mut incoming = IncomingClipboard::new();
        incoming.begin_offer();
        incoming.advertise("text/plain");
        incoming.advertise("image/png");
        incoming.advertise("text/uri-list");
        assert_eq!(
            incoming.requested_mimes(),
            ["image/png", "text/uri-list", "text/plain"]
        );
    }

    #[test]
    fn collects_only_requested_transfers_and_completes() {
        let mut incoming = IncomingClipboard::new();
        incoming.begin_offer();
        incoming.advertise("text/plain");
        assert_eq!(incoming.begin_receiving(), ["text/plain"]);
        incoming.receive("text/plain", b"hello".to_vec());
        assert!(incoming.is_complete());
        assert_eq!(
            incoming.payload().requested("text/plain"),
            Some(&b"hello"[..])
        );
    }

    #[test]
    fn duplicate_advertisements_do_not_duplicate_requests() {
        let mut incoming = IncomingClipboard::new();
        incoming.begin_offer();
        incoming.advertise("text/plain");
        incoming.advertise("text/plain");
        assert_eq!(incoming.begin_receiving(), ["text/plain"]);
    }
}
