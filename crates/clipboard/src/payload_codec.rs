//! Serialises one clipboard payload to the bytes of its entry file.

use crate::byte_codec::{ByteReader, ByteWriter};
use crate::{ClipboardOffer, ClipboardPayload, HistoryStoreError};

const MAGIC: [u8; 4] = *b"CLPE";
const VERSION: u8 = 1;

/// Lossless encoding of every MIME representation in a payload.
pub struct PayloadCodec;

impl PayloadCodec {
    pub fn encode(payload: &ClipboardPayload) -> Result<Vec<u8>, HistoryStoreError> {
        let mut writer = ByteWriter::new();
        writer.tag(MAGIC, VERSION);
        Self::write_offers(&mut writer, payload)?;
        Ok(writer.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<ClipboardPayload, HistoryStoreError> {
        let mut reader = ByteReader::new(bytes);
        reader.expect_tag(MAGIC, VERSION)?;
        let payload = Self::read_offers(&mut reader)?;
        reader.finish()?;
        Ok(payload)
    }

    pub fn write_offers(
        writer: &mut ByteWriter,
        payload: &ClipboardPayload,
    ) -> Result<(), HistoryStoreError> {
        writer.count(payload.offers().len())?;
        for offer in payload.offers() {
            writer.field(offer.mime().as_bytes())?;
            writer.field(offer.bytes())?;
        }
        Ok(())
    }

    pub fn read_offers(reader: &mut ByteReader<'_>) -> Result<ClipboardPayload, HistoryStoreError> {
        let count = reader.count()?;
        let mut payload = ClipboardPayload::new();
        for _ in 0..count {
            let mime = reader.utf8()?;
            payload.offer(ClipboardOffer::new(mime, reader.field()?.to_vec()));
        }
        if payload.is_empty() {
            return Err(HistoryStoreError::invalid("empty payload"));
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::PayloadCodec;
    use crate::{ClipboardOffer, ClipboardPayload};

    #[test]
    fn round_trip_preserves_mimes_order_and_bytes() {
        let mut payload = ClipboardPayload::new();
        payload.offer(ClipboardOffer::new("image/png", vec![0, 1, 255]));
        payload.offer(ClipboardOffer::new("text/plain", b"nyaa".to_vec()));
        let bytes = PayloadCodec::encode(&payload).unwrap();
        assert_eq!(PayloadCodec::decode(&bytes).unwrap(), payload);
    }

    #[test]
    fn empty_and_foreign_files_are_rejected() {
        let empty = PayloadCodec::encode(&ClipboardPayload::new()).unwrap();
        assert!(PayloadCodec::decode(&empty).is_err());
        assert!(PayloadCodec::decode(b"CLPH\x01").is_err());
    }
}
