//! One file per history entry, holding that entry's full payload.

use crate::payload_codec::PayloadCodec;
use crate::{ClipboardPayload, HistoryStoreError};
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const EXTENSION: &str = "bin";

/// A directory of `<id>.bin` payload files.
pub struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write a payload atomically through a temporary sibling and rename.
    pub fn write(&self, id: u64, payload: &ClipboardPayload) -> Result<(), HistoryStoreError> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path(id);
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, PayloadCodec::encode(payload)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn read(&self, id: u64) -> Result<ClipboardPayload, HistoryStoreError> {
        PayloadCodec::decode(&fs::read(self.path(id))?)
    }

    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.path(id).is_file()
    }

    /// Delete one entry; a file that is already gone is not an error.
    pub fn remove(&self, id: u64) -> Result<(), HistoryStoreError> {
        match fs::remove_file(self.path(id)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    /// Delete every entry file whose id is not in `live`.
    pub fn sweep(&self, live: &HashSet<u64>) -> Result<(), HistoryStoreError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if Self::id_of(&path).is_none_or(|id| !live.contains(&id)) {
                fs::remove_file(&path)?;
            }
        }
        Ok(())
    }

    /// Remove the whole directory.
    pub fn clear(&self) -> Result<(), HistoryStoreError> {
        match fs::remove_dir_all(&self.dir) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.{EXTENSION}"))
    }

    fn id_of(path: &Path) -> Option<u64> {
        if path.extension()? != EXTENSION {
            return None;
        }
        path.file_stem()?.to_str()?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::BlobStore;
    use crate::{ClipboardOffer, ClipboardPayload};
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct BlobFixture;

    impl BlobFixture {
        fn store() -> BlobStore {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            BlobStore::new(std::env::temp_dir().join(format!("clipboard-blobs-{stamp}")))
        }

        fn payload(byte: u8) -> ClipboardPayload {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new("text/plain", vec![byte]));
            payload
        }
    }

    #[test]
    fn write_read_remove_round_trip() {
        let store = BlobFixture::store();
        store.write(3, &BlobFixture::payload(7)).unwrap();
        assert!(store.contains(3));
        assert_eq!(store.read(3).unwrap(), BlobFixture::payload(7));
        store.remove(3).unwrap();
        store.remove(3).unwrap();
        assert!(!store.contains(3));
        store.clear().unwrap();
    }

    #[test]
    fn sweep_deletes_only_dead_and_foreign_files() {
        let store = BlobFixture::store();
        store.write(1, &BlobFixture::payload(1)).unwrap();
        store.write(2, &BlobFixture::payload(2)).unwrap();
        std::fs::write(store.dir().join("stray.tmp"), b"x").unwrap();
        store.sweep(&HashSet::from([2])).unwrap();
        assert!(!store.contains(1));
        assert!(store.contains(2));
        assert!(!store.dir().join("stray.tmp").exists());
        store.clear().unwrap();
        assert!(!store.dir().exists());
    }
}
