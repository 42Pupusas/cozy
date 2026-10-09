//! A whole clip of packed frames held in memory.

use std::path::Path;

use crate::mono_frame::MonoFrame;
use crate::pack_header::{HEADER_BYTES, HeaderError, PackHeader};
use crate::unpack_table::UnpackTable;

/// Every frame of one clip, packed at one bit per pixel.
pub struct FramePack {
    header: PackHeader,
    frames: Vec<u8>,
    table: UnpackTable<u32>,
}

impl FramePack {
    /// Load a pack file written by the prepare tool.
    pub fn load(path: &Path, unlit: u32, lit: u32) -> Result<Self, PackError> {
        let bytes = std::fs::read(path).map_err(PackError::Read)?;
        Self::from_bytes(&bytes, unlit, lit)
    }

    /// Build a pack from an in-memory file image.
    pub fn from_bytes(bytes: &[u8], unlit: u32, lit: u32) -> Result<Self, PackError> {
        let header = PackHeader::parse(bytes).map_err(PackError::Header)?;
        let frames = bytes[HEADER_BYTES..].to_vec();
        let expected = header
            .packed_frame_bytes()
            .checked_mul(header.frame_count as usize)
            .ok_or(PackError::SizeOverflow)?;
        if frames.len() != expected {
            return Err(PackError::LengthMismatch {
                expected,
                found: frames.len(),
            });
        }
        Ok(Self {
            header,
            frames,
            table: UnpackTable::new(unlit, lit),
        })
    }

    /// The clip geometry and nominal rate.
    #[must_use]
    pub const fn header(&self) -> PackHeader {
        self.header
    }

    /// How many frames the clip holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.header.frame_count as usize
    }

    /// Whether the clip holds no frames.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.header.frame_count == 0
    }

    /// The expansion table shared by every frame of this pack.
    #[must_use]
    pub const fn table(&self) -> &UnpackTable<u32> {
        &self.table
    }

    /// Frame `index`, wrapping so playback loops forever.
    #[must_use]
    pub fn frame(&self, index: usize) -> Option<MonoFrame<'_>> {
        if self.is_empty() {
            return None;
        }
        let stride = self.header.packed_frame_bytes();
        let start = index % self.len() * stride;
        Some(MonoFrame::new(
            &self.frames[start..start + stride],
            self.header,
        ))
    }

    /// Expand frame `index` into `out`, scaling to `width` by `height`.
    pub fn render_into(&self, index: usize, width: u32, height: u32, out: &mut [u32]) {
        let Some(frame) = self.frame(index) else {
            out.fill(0);
            return;
        };
        if width == self.header.width && height == self.header.height {
            frame.expand_into(&self.table, out);
        } else {
            frame.scale_into(&self.table, width, height, out);
        }
    }
}

/// Why a pack file could not be loaded.
#[derive(Debug)]
pub enum PackError {
    Read(std::io::Error),
    Header(HeaderError),
    SizeOverflow,
    LengthMismatch { expected: usize, found: usize },
}

impl core::fmt::Display for PackError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "failed to read pack file: {error}"),
            Self::Header(error) => write!(formatter, "invalid pack header: {error}"),
            Self::SizeOverflow => write!(formatter, "pack frame count overflows addressable size"),
            Self::LengthMismatch { expected, found } => write!(
                formatter,
                "pack holds {found} frame bytes but its header describes {expected}"
            ),
        }
    }
}

impl std::error::Error for PackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::Header(error) => Some(error),
            Self::SizeOverflow | Self::LengthMismatch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePack, PackError, PackHeader};

    fn pack_bytes(frames: &[[u8; 2]]) -> Vec<u8> {
        let header = PackHeader::new(4, 2, u32::try_from(frames.len()).unwrap(), 30).unwrap();
        let mut bytes = header.to_bytes().to_vec();
        for frame in frames {
            bytes.extend_from_slice(frame);
        }
        bytes
    }

    #[test]
    fn loads_and_reports_its_geometry() {
        let pack = FramePack::from_bytes(&pack_bytes(&[[0xFF, 0x00]]), 0, 1).unwrap();
        assert_eq!(pack.len(), 1);
        assert_eq!(pack.header().width, 4);
        assert!(!pack.is_empty());
    }

    #[test]
    fn playback_wraps_past_the_final_frame() {
        let pack = FramePack::from_bytes(&pack_bytes(&[[0xF0, 0x00], [0x00, 0xF0]]), 0, 1).unwrap();
        assert!(pack.frame(0).unwrap().pixel(0, 0));
        assert!(!pack.frame(1).unwrap().pixel(0, 0));
        assert!(pack.frame(2).unwrap().pixel(0, 0));
    }

    #[test]
    fn rejects_a_truncated_frame_region() {
        let mut bytes = pack_bytes(&[[0xFF, 0x00]]);
        bytes.pop();
        assert!(matches!(
            FramePack::from_bytes(&bytes, 0, 1),
            Err(PackError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn renders_native_and_scaled_sizes() {
        let pack = FramePack::from_bytes(&pack_bytes(&[[0b1010_0000, 0b0101_0000]]), 0, 1).unwrap();
        let mut native = [0u32; 8];
        pack.render_into(0, 4, 2, &mut native);
        assert_eq!(native, [1, 0, 1, 0, 0, 1, 0, 1]);
        let mut scaled = [0u32; 32];
        pack.render_into(0, 8, 4, &mut scaled);
        assert_eq!(scaled[0..8], [1, 1, 0, 0, 1, 1, 0, 0]);
    }

    #[test]
    fn an_empty_pack_renders_blank_frames() {
        let pack = FramePack::from_bytes(&pack_bytes(&[]), 0, 1).unwrap();
        assert!(pack.is_empty());
        let mut out = [7u32; 8];
        pack.render_into(0, 4, 2, &mut out);
        assert_eq!(out, [0u32; 8]);
    }
}
