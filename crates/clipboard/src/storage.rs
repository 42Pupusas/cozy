//! Lossless, versioned persistence for clipboard history.

use crate::{ClipboardHistory, ClipboardOffer, ClipboardPayload};
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"CLPH";
const VERSION: u8 = 1;

/// Errors produced while loading or saving history.
#[derive(Debug)]
pub enum HistoryStoreError {
    /// The filesystem rejected an operation.
    Io(std::io::Error),
    /// The file is malformed or uses an unsupported format.
    InvalidFormat(String),
}

impl Display for HistoryStoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "clipboard history I/O failed: {error}"),
            Self::InvalidFormat(message) => {
                write!(formatter, "invalid clipboard history: {message}")
            }
        }
    }
}

impl std::error::Error for HistoryStoreError {}

impl From<std::io::Error> for HistoryStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads and atomically writes one clipboard-history file.
pub struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    /// Open a store at `path` without touching the filesystem.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load a history, returning an empty one when the file does not exist.
    pub fn load(&self, capacity: usize) -> Result<ClipboardHistory, HistoryStoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Self::decode(&bytes, capacity),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ClipboardHistory::with_capacity(capacity))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Save history through a sibling temporary file and rename.
    pub fn save(&self, history: &ClipboardHistory) -> Result<(), HistoryStoreError> {
        let bytes = Self::encode(history)?;
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.path)?;
        Ok(())
    }

    fn encode(history: &ClipboardHistory) -> Result<Vec<u8>, HistoryStoreError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&Self::length(history.entries().len())?.to_le_bytes());
        for entry in history.entries() {
            bytes.extend_from_slice(&entry.id().to_le_bytes());
            bytes.extend_from_slice(&Self::length(entry.payload().offers().len())?.to_le_bytes());
            for offer in entry.payload().offers() {
                Self::write_bytes(&mut bytes, offer.mime().as_bytes())?;
                Self::write_bytes(&mut bytes, offer.bytes())?;
            }
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8], capacity: usize) -> Result<ClipboardHistory, HistoryStoreError> {
        let mut reader = Cursor::new(bytes);
        if Self::read_exact(&mut reader, 4)? != MAGIC {
            return Err(Self::invalid("bad magic"));
        }
        if Self::read_u8(&mut reader)? != VERSION {
            return Err(Self::invalid("unsupported version"));
        }
        let count = Self::read_u32(&mut reader)? as usize;
        let mut history = ClipboardHistory::with_capacity(capacity);
        for _ in 0..count {
            let id = Self::read_u64(&mut reader)?;
            let offers = Self::read_u32(&mut reader)? as usize;
            let mut payload = ClipboardPayload::new();
            for _ in 0..offers {
                let mime = String::from_utf8(Self::read_bytes(&mut reader)?)
                    .map_err(|_| Self::invalid("MIME is not UTF-8"))?;
                payload.offer(ClipboardOffer::new(mime, Self::read_bytes(&mut reader)?));
            }
            if payload.is_empty() {
                return Err(Self::invalid("empty payload"));
            }
            history.restore(id, payload);
        }
        if reader.position() != bytes.len() as u64 {
            return Err(Self::invalid("trailing bytes"));
        }
        Ok(history)
    }

    fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), HistoryStoreError> {
        output.extend_from_slice(&Self::length(bytes.len())?.to_le_bytes());
        output.extend_from_slice(bytes);
        Ok(())
    }

    fn length(length: usize) -> Result<u32, HistoryStoreError> {
        u32::try_from(length).map_err(|_| Self::invalid("field is too large"))
    }

    fn read_exact(reader: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<u8>, HistoryStoreError> {
        let mut bytes = vec![0; count];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| Self::invalid("truncated file"))?;
        Ok(bytes)
    }

    fn read_bytes(reader: &mut Cursor<&[u8]>) -> Result<Vec<u8>, HistoryStoreError> {
        let length = Self::read_u32(reader)? as usize;
        if length > 64 * 1024 * 1024 {
            return Err(Self::invalid("field is too large"));
        }
        Self::read_exact(reader, length)
    }

    fn read_u8(reader: &mut Cursor<&[u8]>) -> Result<u8, HistoryStoreError> {
        Ok(Self::read_exact(reader, 1)?[0])
    }

    fn read_u32(reader: &mut Cursor<&[u8]>) -> Result<u32, HistoryStoreError> {
        let bytes = Self::read_exact(reader, 4)?;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_u64(reader: &mut Cursor<&[u8]>) -> Result<u64, HistoryStoreError> {
        let bytes = Self::read_exact(reader, 8)?;
        Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn invalid(message: &str) -> HistoryStoreError {
        HistoryStoreError::InvalidFormat(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryStore;
    use crate::{ClipboardHistory, ClipboardOffer, ClipboardPayload};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct StorageFixture;

    impl StorageFixture {
        fn path() -> std::path::PathBuf {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            std::env::temp_dir().join(format!("clipboard-history-{stamp}.bin"))
        }

        fn payload() -> ClipboardPayload {
            let mut payload = ClipboardPayload::new();
            payload.offer(ClipboardOffer::new(
                "text/plain;charset=utf-8",
                b"nyaa".to_vec(),
            ));
            payload.offer(ClipboardOffer::new("image/png", vec![0, 1, 2, 255]));
            payload
        }
    }

    #[test]
    fn round_trip_preserves_ids_order_mimes_and_bytes() {
        let path = StorageFixture::path();
        let store = HistoryStore::new(&path);
        let mut history = ClipboardHistory::with_capacity(4);
        history.push(StorageFixture::payload());
        store.save(&history).unwrap();
        let loaded = store.load(4).unwrap();
        assert_eq!(loaded, history);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_file_is_an_empty_history() {
        let store = HistoryStore::new(StorageFixture::path());
        assert!(store.load(3).unwrap().entries().is_empty());
    }

    #[test]
    fn malformed_file_is_rejected() {
        let path = StorageFixture::path();
        std::fs::write(&path, b"not clipboard history").unwrap();
        let error = HistoryStore::new(&path).load(3).unwrap_err();
        assert!(error.to_string().contains("bad magic"));
        let _ = std::fs::remove_file(path);
    }
}
