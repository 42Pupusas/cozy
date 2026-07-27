//! Reusable outgoing clipboard payloads.
//!
//! This crate deliberately knows nothing about Wayland transport. It owns the
//! data policy shared by clipboard clients: MIME representations, replacement,
//! and exact request lookup. The Wayland owner can remain a thin adapter while
//! these rules stay headlessly testable.

mod history;
mod incoming;
mod offer_registry;
mod storage;

pub use history::{ClipboardHistory, HistoryEntry};
pub use incoming::{IncomingClipboard, IncomingState};
pub use offer_registry::OfferRegistry;
pub use storage::{HistoryStore, HistoryStoreError};

/// A byte payload advertised under one MIME type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardOffer {
    mime: String,
    bytes: Vec<u8>,
}

impl ClipboardOffer {
    /// Construct an offer.
    #[must_use]
    pub fn new(mime: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            mime: mime.into(),
            bytes: bytes.into(),
        }
    }

    /// The advertised MIME type.
    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// The bytes to write when a peer requests this offer.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Owned clipboard contents, including every advertised representation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardPayload {
    offers: Vec<ClipboardOffer>,
}

impl ClipboardPayload {
    /// Start an empty payload.
    #[must_use]
    pub const fn new() -> Self {
        Self { offers: Vec::new() }
    }

    /// Add or replace one MIME representation.
    pub fn offer(&mut self, offer: ClipboardOffer) {
        if let Some(existing) = self
            .offers
            .iter_mut()
            .find(|existing| existing.mime == offer.mime)
        {
            *existing = offer;
        } else {
            self.offers.push(offer);
        }
    }

    /// Find the exact representation requested by a peer.
    #[must_use]
    pub fn requested(&self, mime: &str) -> Option<&[u8]> {
        self.offers
            .iter()
            .find(|offer| offer.mime == mime)
            .map(ClipboardOffer::bytes)
    }

    /// Advertised MIME names in stable insertion order.
    #[must_use]
    pub fn mimes(&self) -> impl ExactSizeIterator<Item = &str> {
        self.offers.iter().map(|offer| offer.mime.as_str())
    }

    /// All representations in stable insertion order.
    #[must_use]
    pub(crate) fn offers(&self) -> &[ClipboardOffer] {
        &self.offers
    }

    /// Whether no data can be offered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }

    /// Number of MIME representations.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.offers.len()
    }
}

/// A request from the compositor to the clipboard owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerRequest {
    /// The peer asks for one advertised representation.
    Send(String),
    /// The compositor reports that another source replaced this owner.
    Cancelled,
}

/// The result of handling one owner request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerResponse<'a> {
    /// Serve these bytes on the fd associated with the request.
    Bytes(&'a [u8]),
    /// Stop the owner; its selection is no longer current.
    Stop,
    /// The requested MIME type was not advertised.
    Unsupported,
}

/// Decides what a persistent owner should do with compositor callbacks.
pub struct ClipboardOwner<'a> {
    payload: &'a ClipboardPayload,
}

impl<'a> ClipboardOwner<'a> {
    /// Attach the owner decision layer to a payload held by the process.
    #[must_use]
    pub const fn new(payload: &'a ClipboardPayload) -> Self {
        Self { payload }
    }

    /// Handle a request without doing I/O itself.
    #[must_use]
    pub fn handle(&self, request: OwnerRequest) -> OwnerResponse<'a> {
        match request {
            OwnerRequest::Send(mime) => self
                .payload
                .requested(&mime)
                .map_or(OwnerResponse::Unsupported, OwnerResponse::Bytes),
            OwnerRequest::Cancelled => OwnerResponse::Stop,
        }
    }
}

/// MIME representations commonly useful for a screenshot file.
pub struct ScreenshotPayload;

impl ScreenshotPayload {
    /// Build image and file-oriented clipboard representations.
    #[must_use]
    pub fn from_png(png: Vec<u8>, file_uri: impl Into<String>) -> ClipboardPayload {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", png));
        let uri = format!("{}\n", file_uri.into());
        payload.offer(ClipboardOffer::new("text/uri-list", uri.into_bytes()));
        payload
    }
}

#[cfg(test)]
mod tests {
    use super::{ClipboardOffer, ClipboardPayload, ScreenshotPayload};

    #[test]
    fn replacement_keeps_one_representation_per_mime() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", vec![1]));
        payload.offer(ClipboardOffer::new("text/plain", vec![2]));
        payload.offer(ClipboardOffer::new("image/png", vec![3]));
        assert_eq!(payload.len(), 2);
        assert_eq!(payload.requested("image/png"), Some(&[3][..]));
        assert_eq!(
            payload.mimes().collect::<Vec<_>>(),
            ["image/png", "text/plain"]
        );
    }

    #[test]
    fn unknown_mime_is_not_silently_substituted() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", vec![1]));
        assert_eq!(payload.requested("image/jpeg"), None);
    }

    #[test]
    fn owner_serves_only_advertised_mimes_and_stops_on_replacement() {
        let payload = ScreenshotPayload::from_png(vec![1, 2], "file:///tmp/shot.png");
        let owner = super::ClipboardOwner::new(&payload);
        assert_eq!(
            owner.handle(super::OwnerRequest::Send("image/png".into())),
            super::OwnerResponse::Bytes(&[1, 2])
        );
        assert_eq!(
            owner.handle(super::OwnerRequest::Send("image/jpeg".into())),
            super::OwnerResponse::Unsupported
        );
        assert_eq!(
            owner.handle(super::OwnerRequest::Cancelled),
            super::OwnerResponse::Stop
        );
    }

    #[test]
    fn screenshot_payload_supports_image_and_file_pasting() {
        let payload = ScreenshotPayload::from_png(vec![1, 2], "file:///tmp/shot.png");
        assert_eq!(payload.requested("image/png"), Some(&[1, 2][..]));
        assert_eq!(
            payload.requested("text/uri-list"),
            Some(&b"file:///tmp/shot.png\n"[..])
        );
    }
}
