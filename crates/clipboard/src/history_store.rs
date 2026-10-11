//! Clipboard history on disk: a small index file plus one file per payload.
//!
//! Only the index is ever loaded whole. Payload bytes are written once when
//! an entry is recorded and read back only for the entry being pasted or
//! decoded, so a long-running daemon's memory does not grow with the size of
//! the images in its history.

use crate::entry_index::IndexEntry;
use crate::index_codec::IndexCodec;
use crate::legacy_history::{self, LegacyHistory};
use crate::{
    Admission, BlobStore, ClipboardPayload, EntryIndex, EntrySummary, HistoryStoreError,
    PayloadDigest,
};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The index file at `path` and its payload directory `<path>.entries`.
pub struct HistoryStore {
    path: PathBuf,
    blobs: BlobStore,
}

impl HistoryStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut dir = OsString::from(path.as_os_str());
        dir.push(".entries");
        Self {
            path,
            blobs: BlobStore::new(dir),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the index, upgrading a legacy single-file history in place.
    ///
    /// Entries whose payload file is missing are dropped, so a half-finished
    /// write can never surface as an entry that fails to paste.
    pub fn load(&self, capacity: usize) -> Result<EntryIndex, HistoryStoreError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(EntryIndex::with_capacity(capacity));
            }
            Err(error) => return Err(error.into()),
        };
        let mut index = if bytes.starts_with(&legacy_history::MAGIC) {
            self.migrate(&bytes, capacity)?
        } else {
            IndexCodec::decode(&bytes, capacity)?
        };
        index.retain(|id| self.blobs.contains(id));
        Ok(index)
    }

    /// Record a payload as the newest entry and persist the change.
    ///
    /// Returns the entry's id, or `None` when the payload is empty or the
    /// history has no capacity.
    pub fn record(
        &self,
        index: &mut EntryIndex,
        payload: &ClipboardPayload,
    ) -> Result<Option<u64>, HistoryStoreError> {
        if payload.is_empty() {
            return Ok(None);
        }
        let Some(admission) = index.record(
            PayloadDigest::of(payload),
            EntrySummary::of(payload),
            |id| self.blobs.read(id).is_ok_and(|stored| &stored == payload),
        ) else {
            return Ok(None);
        };
        if let Admission::Inserted { id, .. } = &admission
            && let Err(error) = self.blobs.write(*id, payload)
        {
            let failed = *id;
            index.retain(|id| id != failed);
            return Err(error);
        }
        self.save(index)?;
        if let Admission::Inserted { evicted, .. } = &admission {
            for id in evicted {
                self.blobs.remove(*id)?;
            }
        }
        Ok(Some(admission.id()))
    }

    /// The full payload of one entry.
    pub fn payload(&self, entry: &IndexEntry) -> Result<ClipboardPayload, HistoryStoreError> {
        self.blobs.read(entry.id())
    }

    /// Delete payload files that no index entry refers to.
    pub fn sweep(&self, index: &EntryIndex) -> Result<(), HistoryStoreError> {
        let live: HashSet<u64> = index.stored().iter().map(IndexEntry::id).collect();
        self.blobs.sweep(&live)
    }

    /// Forget every entry.
    pub fn clear(&self) -> Result<(), HistoryStoreError> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        self.blobs.clear()
    }

    fn save(&self, index: &EntryIndex) -> Result<(), HistoryStoreError> {
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, IndexCodec::encode(index)?)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }

    fn migrate(&self, bytes: &[u8], capacity: usize) -> Result<EntryIndex, HistoryStoreError> {
        let mut entries = Vec::new();
        LegacyHistory::for_each(bytes, |id, payload| {
            self.blobs.write(id, &payload)?;
            entries.push(IndexEntry::new(
                id,
                PayloadDigest::of(&payload),
                EntrySummary::of(&payload),
            ));
            Ok(())
        })?;
        let index = EntryIndex::restore(capacity, 0, entries);
        self.save(&index)?;
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryStore;
    use crate::legacy_history::LegacyFixture;
    use crate::{ClipboardOffer, ClipboardPayload};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct StoreFixture;

    impl StoreFixture {
        fn store() -> HistoryStore {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            HistoryStore::new(std::env::temp_dir().join(format!("clipboard-history-{stamp}.bin")))
        }

        fn payload(byte: u8) -> ClipboardPayload {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new("text/plain", vec![byte]));
            payload.offer(ClipboardOffer::new("image/png", vec![0, byte, 255]));
            payload
        }

        fn ids(store: &HistoryStore, capacity: usize) -> Vec<u64> {
            store
                .load(capacity)
                .unwrap()
                .entries()
                .iter()
                .map(crate::IndexEntry::id)
                .collect()
        }
    }

    #[test]
    fn recorded_entries_survive_a_reload_with_their_bytes() {
        let store = StoreFixture::store();
        let mut index = store.load(4).unwrap();
        assert_eq!(
            store.record(&mut index, &StoreFixture::payload(1)).unwrap(),
            Some(0)
        );
        assert_eq!(
            store.record(&mut index, &StoreFixture::payload(2)).unwrap(),
            Some(1)
        );
        let reloaded = store.load(4).unwrap();
        assert_eq!(reloaded, index);
        let newest = reloaded.get(1).unwrap();
        assert_eq!(store.payload(newest).unwrap(), StoreFixture::payload(2));
        store.clear().unwrap();
    }

    #[test]
    fn eviction_deletes_the_payload_file() {
        let store = StoreFixture::store();
        let mut index = store.load(1).unwrap();
        store.record(&mut index, &StoreFixture::payload(1)).unwrap();
        store.record(&mut index, &StoreFixture::payload(2)).unwrap();
        assert!(!store.blobs.contains(0));
        assert_eq!(StoreFixture::ids(&store, 1), [1]);
        store.clear().unwrap();
    }

    #[test]
    fn duplicate_payload_is_promoted_not_stored_twice() {
        let store = StoreFixture::store();
        let mut index = store.load(4).unwrap();
        store.record(&mut index, &StoreFixture::payload(1)).unwrap();
        store.record(&mut index, &StoreFixture::payload(2)).unwrap();
        assert_eq!(
            store.record(&mut index, &StoreFixture::payload(1)).unwrap(),
            Some(0)
        );
        assert_eq!(StoreFixture::ids(&store, 4), [0, 1]);
        store.clear().unwrap();
    }

    #[test]
    fn empty_payload_is_ignored() {
        let store = StoreFixture::store();
        let mut index = store.load(4).unwrap();
        assert_eq!(
            store.record(&mut index, &ClipboardPayload::new()).unwrap(),
            None
        );
        assert!(!store.path().exists());
    }

    #[test]
    fn missing_file_is_an_empty_history() {
        assert!(StoreFixture::store().load(3).unwrap().entries().is_empty());
    }

    #[test]
    fn malformed_file_is_rejected() {
        let store = StoreFixture::store();
        std::fs::write(store.path(), b"not clipboard history").unwrap();
        assert!(store.load(3).unwrap_err().to_string().contains("bad magic"));
        store.clear().unwrap();
    }

    #[test]
    fn entry_with_missing_payload_file_is_dropped() {
        let store = StoreFixture::store();
        let mut index = store.load(4).unwrap();
        store.record(&mut index, &StoreFixture::payload(1)).unwrap();
        store.record(&mut index, &StoreFixture::payload(2)).unwrap();
        store.blobs.remove(0).unwrap();
        assert_eq!(StoreFixture::ids(&store, 4), [1]);
        store.clear().unwrap();
    }

    #[test]
    fn legacy_file_is_migrated_once_and_keeps_order_and_ids() {
        let store = StoreFixture::store();
        let legacy =
            LegacyFixture::encode(&[(9, StoreFixture::payload(9)), (4, StoreFixture::payload(4))]);
        std::fs::write(store.path(), legacy).unwrap();
        assert_eq!(StoreFixture::ids(&store, 10), [9, 4]);
        assert!(!std::fs::read(store.path()).unwrap().starts_with(b"CLPH"));
        let mut index = store.load(10).unwrap();
        let oldest = index.get(4).unwrap();
        assert_eq!(store.payload(oldest).unwrap(), StoreFixture::payload(4));
        assert_eq!(
            store.record(&mut index, &StoreFixture::payload(1)).unwrap(),
            Some(10)
        );
        store.clear().unwrap();
    }

    #[test]
    fn sweep_removes_orphaned_payload_files() {
        let store = StoreFixture::store();
        let mut index = store.load(4).unwrap();
        store.record(&mut index, &StoreFixture::payload(1)).unwrap();
        store.blobs.write(77, &StoreFixture::payload(7)).unwrap();
        store.sweep(&index).unwrap();
        assert!(!store.blobs.contains(77));
        assert!(store.blobs.contains(0));
        store.clear().unwrap();
    }
}
