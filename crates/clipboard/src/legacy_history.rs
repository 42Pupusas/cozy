//! Reader for the original single-file history format (`CLPH` v1).
//!
//! That format stored every payload inline. It is only read, once, to move
//! its entries into the index-plus-blobs layout.

use crate::byte_codec::ByteReader;
use crate::payload_codec::PayloadCodec;
use crate::{ClipboardPayload, HistoryStoreError};

pub const MAGIC: [u8; 4] = *b"CLPH";
const VERSION: u8 = 1;

/// Streams `(id, payload)` pairs, newest first, out of a legacy file.
pub struct LegacyHistory;

impl LegacyHistory {
    pub fn for_each(
        bytes: &[u8],
        mut visit: impl FnMut(u64, ClipboardPayload) -> Result<(), HistoryStoreError>,
    ) -> Result<(), HistoryStoreError> {
        let mut reader = ByteReader::new(bytes);
        reader.expect_tag(MAGIC, VERSION)?;
        let count = reader.count()?;
        for _ in 0..count {
            let id = reader.u64()?;
            visit(id, PayloadCodec::read_offers(&mut reader)?)?;
        }
        reader.finish()
    }
}

#[cfg(test)]
pub struct LegacyFixture;

#[cfg(test)]
impl LegacyFixture {
    /// Encode entries exactly as the retired `HistoryStore::encode` did.
    pub fn encode(entries: &[(u64, ClipboardPayload)]) -> Vec<u8> {
        let mut writer = crate::byte_codec::ByteWriter::new();
        writer.tag(MAGIC, VERSION);
        writer.count(entries.len()).unwrap();
        for (id, payload) in entries {
            writer.u64(*id);
            PayloadCodec::write_offers(&mut writer, payload).unwrap();
        }
        writer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{LegacyFixture, LegacyHistory};
    use crate::{ClipboardOffer, ClipboardPayload};

    #[test]
    fn entries_are_visited_in_file_order() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("text/plain", b"x".to_vec()));
        let bytes = LegacyFixture::encode(&[(5, payload.clone()), (2, payload.clone())]);
        let mut seen = Vec::new();
        LegacyHistory::for_each(&bytes, |id, decoded| {
            assert_eq!(decoded, payload);
            seen.push(id);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [5, 2]);
    }
}
