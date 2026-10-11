//! Little-endian, length-prefixed primitives shared by every history file format.

use crate::HistoryStoreError;

const FIELD_LIMIT: usize = 64 * 1024 * 1024;

/// Appends tagged, length-prefixed fields to a buffer.
pub struct ByteWriter {
    bytes: Vec<u8>,
}

impl ByteWriter {
    pub const fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn tag(&mut self, magic: [u8; 4], version: u8) {
        self.bytes.extend_from_slice(&magic);
        self.bytes.push(version);
    }

    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn count(&mut self, count: usize) -> Result<(), HistoryStoreError> {
        let count =
            u32::try_from(count).map_err(|_| HistoryStoreError::invalid("field is too large"))?;
        self.bytes.extend_from_slice(&count.to_le_bytes());
        Ok(())
    }

    pub fn field(&mut self, bytes: &[u8]) -> Result<(), HistoryStoreError> {
        self.count(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads fields written by [`ByteWriter`], rejecting truncation and trailing bytes.
pub struct ByteReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    pub fn expect_tag(&mut self, magic: [u8; 4], version: u8) -> Result<(), HistoryStoreError> {
        if self.take(4)? != magic.as_slice() {
            return Err(HistoryStoreError::invalid("bad magic"));
        }
        if self.u8()? != version {
            return Err(HistoryStoreError::invalid("unsupported version"));
        }
        Ok(())
    }

    pub fn u8(&mut self) -> Result<u8, HistoryStoreError> {
        Ok(self.take(1)?[0])
    }

    pub fn u64(&mut self) -> Result<u64, HistoryStoreError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().expect("take returned 8 bytes");
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn count(&mut self) -> Result<usize, HistoryStoreError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("take returned 4 bytes");
        Ok(u32::from_le_bytes(bytes) as usize)
    }

    pub fn field(&mut self) -> Result<&'a [u8], HistoryStoreError> {
        let length = self.count()?;
        if length > FIELD_LIMIT {
            return Err(HistoryStoreError::invalid("field is too large"));
        }
        self.take(length)
    }

    pub fn utf8(&mut self) -> Result<String, HistoryStoreError> {
        String::from_utf8(self.field()?.to_vec())
            .map_err(|_| HistoryStoreError::invalid("MIME is not UTF-8"))
    }

    pub fn finish(&self) -> Result<(), HistoryStoreError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(HistoryStoreError::invalid("trailing bytes"))
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], HistoryStoreError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| HistoryStoreError::invalid("truncated file"))?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteReader, ByteWriter};

    #[test]
    fn fields_round_trip() {
        let mut writer = ByteWriter::new();
        writer.tag(*b"TEST", 3);
        writer.u8(9);
        writer.u64(u64::MAX);
        writer.field(b"abc").unwrap();
        let bytes = writer.finish();
        let mut reader = ByteReader::new(&bytes);
        reader.expect_tag(*b"TEST", 3).unwrap();
        assert_eq!(reader.u8().unwrap(), 9);
        assert_eq!(reader.u64().unwrap(), u64::MAX);
        assert_eq!(reader.field().unwrap(), b"abc");
        reader.finish().unwrap();
    }

    #[test]
    fn truncation_and_trailing_bytes_are_rejected() {
        assert!(ByteReader::new(&[1, 2]).u64().is_err());
        let mut reader = ByteReader::new(&[1, 2]);
        reader.u8().unwrap();
        assert!(reader.finish().is_err());
    }
}
