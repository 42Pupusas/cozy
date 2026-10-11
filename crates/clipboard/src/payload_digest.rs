//! A stable content fingerprint for clipboard payloads.

use crate::ClipboardPayload;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a over every MIME name and its bytes, in offer order.
///
/// Persisted in the index, so it must not depend on the std hasher, whose
/// output may change between Rust releases. Equal digests only nominate a
/// duplicate; the caller confirms against the stored bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PayloadDigest(u64);

impl PayloadDigest {
    #[must_use]
    pub fn of(payload: &ClipboardPayload) -> Self {
        let mut state = FNV_OFFSET;
        for offer in payload.offers() {
            for part in [offer.mime().as_bytes(), offer.bytes()] {
                state = Self::mix(state, &(part.len() as u64).to_le_bytes());
                state = Self::mix(state, part);
            }
        }
        Self(state)
    }

    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    fn mix(mut state: u64, bytes: &[u8]) -> u64 {
        for &byte in bytes {
            state ^= u64::from(byte);
            state = state.wrapping_mul(FNV_PRIME);
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use super::PayloadDigest;
    use crate::{ClipboardOffer, ClipboardPayload};

    struct DigestFixture;

    impl DigestFixture {
        fn payload(mime: &str, bytes: &[u8]) -> ClipboardPayload {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new(mime, bytes.to_vec()));
            payload
        }
    }

    #[test]
    fn equal_payloads_share_a_digest() {
        assert_eq!(
            PayloadDigest::of(&DigestFixture::payload("text/plain", b"a")),
            PayloadDigest::of(&DigestFixture::payload("text/plain", b"a"))
        );
    }

    #[test]
    fn mime_and_bytes_both_contribute() {
        let base = PayloadDigest::of(&DigestFixture::payload("text/plain", b"a"));
        assert_ne!(
            base,
            PayloadDigest::of(&DigestFixture::payload("text/plain", b"b"))
        );
        assert_ne!(
            base,
            PayloadDigest::of(&DigestFixture::payload("text/html", b"a"))
        );
    }

    #[test]
    fn field_boundaries_are_unambiguous() {
        assert_ne!(
            PayloadDigest::of(&DigestFixture::payload("text/plai", b"na")),
            PayloadDigest::of(&DigestFixture::payload("text/plain", b"a"))
        );
    }

    #[test]
    fn known_value_is_stable_across_builds() {
        assert_eq!(
            PayloadDigest::of(&ClipboardPayload::new()).raw(),
            0xcbf2_9ce4_8422_2325
        );
    }
}
