//! Serialises the entry index to the history file.

use crate::byte_codec::{ByteReader, ByteWriter};
use crate::entry_index::IndexEntry;
use crate::{EntryIndex, EntrySummary, HistoryStoreError, MimeSize, PayloadDigest};

const MAGIC: [u8; 4] = *b"CLPI";
const VERSION: u8 = 1;

/// The on-disk format of [`EntryIndex`].
pub struct IndexCodec;

impl IndexCodec {
    pub fn encode(index: &EntryIndex) -> Result<Vec<u8>, HistoryStoreError> {
        let mut writer = ByteWriter::new();
        writer.tag(MAGIC, VERSION);
        writer.u64(index.next_id());
        writer.count(index.stored().len())?;
        for entry in index.stored() {
            writer.u64(entry.id());
            writer.u64(entry.digest().raw());
            Self::write_summary(&mut writer, entry.summary())?;
        }
        Ok(writer.finish())
    }

    pub fn decode(bytes: &[u8], capacity: usize) -> Result<EntryIndex, HistoryStoreError> {
        let mut reader = ByteReader::new(bytes);
        reader.expect_tag(MAGIC, VERSION)?;
        let next_id = reader.u64()?;
        let count = reader.count()?;
        let mut entries = Vec::new();
        for _ in 0..count {
            let id = reader.u64()?;
            let digest = PayloadDigest::from_raw(reader.u64()?);
            entries.push(IndexEntry::new(
                id,
                digest,
                Self::read_summary(&mut reader)?,
            ));
        }
        reader.finish()?;
        Ok(EntryIndex::restore(capacity, next_id, entries))
    }

    fn write_summary(
        writer: &mut ByteWriter,
        summary: &EntrySummary,
    ) -> Result<(), HistoryStoreError> {
        writer.count(summary.mimes().len())?;
        for mime in summary.mimes() {
            writer.field(mime.mime().as_bytes())?;
            writer.u64(mime.len());
        }
        match summary.text_head() {
            Some(head) => {
                writer.u8(1);
                writer.field(head)?;
            }
            None => writer.u8(0),
        }
        Ok(())
    }

    fn read_summary(reader: &mut ByteReader<'_>) -> Result<EntrySummary, HistoryStoreError> {
        let count = reader.count()?;
        let mut mimes = Vec::new();
        for _ in 0..count {
            let mime = reader.utf8()?;
            mimes.push(MimeSize::new(mime, reader.u64()?));
        }
        let text_head = match reader.u8()? {
            0 => None,
            1 => Some(reader.field()?.to_vec()),
            _ => return Err(HistoryStoreError::invalid("bad text-head flag")),
        };
        Ok(EntrySummary::new(mimes, text_head))
    }
}

#[cfg(test)]
mod tests {
    use super::IndexCodec;
    use crate::{ClipboardOffer, ClipboardPayload, EntryIndex, EntrySummary, PayloadDigest};

    #[test]
    fn round_trip_preserves_entries_and_next_id() {
        let mut index = EntryIndex::with_capacity(4);
        for bytes in [b"a".to_vec(), vec![0x89, b'P']] {
            let mut payload = ClipboardPayload::new();
            let mime = if bytes.len() == 1 {
                "text/plain"
            } else {
                "image/png"
            };
            payload.offer(ClipboardOffer::new(mime, bytes));
            index.record(
                PayloadDigest::of(&payload),
                EntrySummary::of(&payload),
                |_| true,
            );
        }
        let decoded = IndexCodec::decode(&IndexCodec::encode(&index).unwrap(), 4).unwrap();
        assert_eq!(decoded, index);
    }

    #[test]
    fn truncated_index_is_rejected() {
        let index = EntryIndex::with_capacity(1);
        let bytes = IndexCodec::encode(&index).unwrap();
        assert!(IndexCodec::decode(&bytes[..bytes.len() - 1], 1).is_err());
    }
}
