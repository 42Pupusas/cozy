//! The fixed-size descriptor at the start of a packed frame file.

/// Byte length of the encoded header.
pub const HEADER_BYTES: usize = 20;

const MAGIC: [u8; 4] = *b"MFP1";

/// Geometry and timing of one packed clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackHeader {
    pub width: u32,
    pub height: u32,
    pub frame_count: u32,
    /// Nominal presentation rate in frames per second.
    pub fps: u32,
}

impl PackHeader {
    /// Build a header, rejecting geometry that cannot describe real frames.
    pub const fn new(
        width: u32,
        height: u32,
        frame_count: u32,
        fps: u32,
    ) -> Result<Self, HeaderError> {
        if width == 0 || height == 0 {
            return Err(HeaderError::EmptyGeometry);
        }
        if fps == 0 {
            return Err(HeaderError::ZeroRate);
        }
        Ok(Self {
            width,
            height,
            frame_count,
            fps,
        })
    }

    /// Packed byte length of one frame, rounding each row up to a byte.
    #[must_use]
    pub const fn packed_frame_bytes(&self) -> usize {
        self.row_bytes() * self.height as usize
    }

    /// Packed byte length of one row.
    #[must_use]
    pub const fn row_bytes(&self) -> usize {
        self.width.div_ceil(8) as usize
    }

    /// Pixel count of one unpacked frame.
    #[must_use]
    pub const fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Encode the header for the start of a pack file.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0u8; HEADER_BYTES];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4..8].copy_from_slice(&self.width.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.height.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.frame_count.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.fps.to_le_bytes());
        bytes
    }

    /// Decode a header from the start of a pack file.
    pub fn parse(bytes: &[u8]) -> Result<Self, HeaderError> {
        let head: &[u8; HEADER_BYTES] = bytes
            .get(..HEADER_BYTES)
            .and_then(|slice| slice.try_into().ok())
            .ok_or(HeaderError::Truncated)?;
        if head[0..4] != MAGIC {
            return Err(HeaderError::BadMagic);
        }
        Self::new(
            Self::field(head, 4),
            Self::field(head, 8),
            Self::field(head, 12),
            Self::field(head, 16),
        )
    }

    fn field(head: &[u8; HEADER_BYTES], offset: usize) -> u32 {
        let mut word = [0u8; 4];
        word.copy_from_slice(&head[offset..offset + 4]);
        u32::from_le_bytes(word)
    }
}

/// Why a pack header could not be built or decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    Truncated,
    BadMagic,
    EmptyGeometry,
    ZeroRate,
}

impl core::fmt::Display for HeaderError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(formatter, "pack file is shorter than its header"),
            Self::BadMagic => write!(formatter, "pack file does not start with the MFP1 magic"),
            Self::EmptyGeometry => write!(formatter, "pack geometry has a zero dimension"),
            Self::ZeroRate => write!(formatter, "pack frame rate is zero"),
        }
    }
}

impl std::error::Error for HeaderError {}

#[cfg(test)]
mod tests {
    use super::{HeaderError, PackHeader};

    #[test]
    fn round_trips_through_bytes() {
        let header = PackHeader::new(480, 360, 9558, 30).unwrap();
        assert_eq!(PackHeader::parse(&header.to_bytes()), Ok(header));
    }

    #[test]
    fn rows_round_up_to_whole_bytes() {
        let header = PackHeader::new(13, 2, 1, 30).unwrap();
        assert_eq!(header.row_bytes(), 2);
        assert_eq!(header.packed_frame_bytes(), 4);
    }

    #[test]
    fn rejects_foreign_files() {
        let mut bytes = PackHeader::new(4, 4, 1, 30).unwrap().to_bytes();
        bytes[0] = b'X';
        assert_eq!(PackHeader::parse(&bytes), Err(HeaderError::BadMagic));
    }

    #[test]
    fn rejects_zero_geometry_and_rate() {
        assert_eq!(
            PackHeader::new(0, 8, 1, 30),
            Err(HeaderError::EmptyGeometry)
        );
        assert_eq!(PackHeader::new(8, 8, 1, 0), Err(HeaderError::ZeroRate));
    }

    #[test]
    fn rejects_a_truncated_file() {
        assert_eq!(PackHeader::parse(&[0u8; 3]), Err(HeaderError::Truncated));
    }
}
