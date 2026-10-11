//! What a picker needs to know about an entry without loading its bytes.

use crate::ClipboardPayload;

/// Text representations a preview draws from, in preference order.
const PREVIEW_TEXT_MIMES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "text/uri-list"];

/// Enough leading text for a one-line preview, with room for leading whitespace.
const TEXT_HEAD_LIMIT: usize = 1024;

/// One advertised representation and its size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MimeSize {
    mime: String,
    len: u64,
}

impl MimeSize {
    #[must_use]
    pub const fn new(mime: String, len: u64) -> Self {
        Self { mime, len }
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// MIME names, sizes, and the start of the preferred text representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySummary {
    mimes: Vec<MimeSize>,
    text_head: Option<Vec<u8>>,
}

impl EntrySummary {
    #[must_use]
    pub const fn new(mimes: Vec<MimeSize>, text_head: Option<Vec<u8>>) -> Self {
        Self { mimes, text_head }
    }

    #[must_use]
    pub fn of(payload: &ClipboardPayload) -> Self {
        let mimes = payload
            .offers()
            .iter()
            .map(|offer| MimeSize::new(offer.mime().to_owned(), offer.bytes().len() as u64))
            .collect();
        let text_head = PREVIEW_TEXT_MIMES
            .into_iter()
            .find_map(|mime| payload.requested(mime))
            .map(Self::head);
        Self { mimes, text_head }
    }

    #[must_use]
    pub fn mimes(&self) -> &[MimeSize] {
        &self.mimes
    }

    #[must_use]
    pub fn size_of(&self, mime: &str) -> Option<u64> {
        self.mimes
            .iter()
            .find(|entry| entry.mime == mime)
            .map(MimeSize::len)
    }

    /// The first bytes of the preferred text representation, cut on a UTF-8 boundary.
    #[must_use]
    pub fn text_head(&self) -> Option<&[u8]> {
        self.text_head.as_deref()
    }

    fn head(bytes: &[u8]) -> Vec<u8> {
        if bytes.len() <= TEXT_HEAD_LIMIT {
            return bytes.to_vec();
        }
        let cut = &bytes[..TEXT_HEAD_LIMIT];
        let end = match std::str::from_utf8(cut) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => TEXT_HEAD_LIMIT,
        };
        cut[..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::{EntrySummary, TEXT_HEAD_LIMIT};
    use crate::{ClipboardOffer, ClipboardPayload};

    #[test]
    fn sizes_and_preferred_text_are_recorded() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", vec![0; 5]));
        payload.offer(ClipboardOffer::new("text/uri-list", b"file:///a".to_vec()));
        payload.offer(ClipboardOffer::new("text/plain", b"a".to_vec()));
        let summary = EntrySummary::of(&payload);
        assert_eq!(summary.size_of("image/png"), Some(5));
        assert_eq!(summary.text_head(), Some(&b"a"[..]));
        assert_eq!(summary.mimes().len(), 3);
    }

    #[test]
    fn long_text_is_cut_on_a_char_boundary() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new(
            "text/plain",
            format!("a{}", "é".repeat(TEXT_HEAD_LIMIT)).into_bytes(),
        ));
        let head = EntrySummary::of(&payload).text_head().unwrap().to_vec();
        assert_eq!(head.len(), TEXT_HEAD_LIMIT - 1);
        assert!(std::str::from_utf8(&head).is_ok());
    }

    #[test]
    fn binary_only_payload_has_no_text_head() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", vec![1]));
        assert!(EntrySummary::of(&payload).text_head().is_none());
    }
}
