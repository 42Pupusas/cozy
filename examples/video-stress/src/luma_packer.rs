//! Raw luma to packed 1-bit conversion.

use crate::pack_header::PackHeader;

/// Turns 8-bit luma frames into packed bits at a fixed threshold.
pub struct LumaPacker {
    header: PackHeader,
    threshold: u8,
}

impl LumaPacker {
    /// Build a packer for `header` geometry at `threshold`.
    #[must_use]
    pub const fn new(header: PackHeader, threshold: u8) -> Self {
        Self { header, threshold }
    }

    /// Pack one luma frame, appending its bytes to `out`.
    ///
    /// `luma` must hold one byte per pixel of the packer's geometry.
    pub fn pack_frame(&self, luma: &[u8], out: &mut Vec<u8>) {
        assert_eq!(luma.len(), self.header.pixel_count());
        let width = self.header.width as usize;
        for row in luma.chunks_exact(width) {
            self.pack_row(row, out);
        }
    }

    /// Pack one row, padding its final byte with unlit bits.
    fn pack_row(&self, row: &[u8], out: &mut Vec<u8>) {
        for group in row.chunks(8) {
            let mut byte = 0u8;
            for (bit, &sample) in group.iter().enumerate() {
                if sample >= self.threshold {
                    byte |= 1 << (7 - bit);
                }
            }
            out.push(byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LumaPacker, PackHeader};

    #[test]
    fn packs_bits_most_significant_first() {
        let header = PackHeader::new(8, 1, 1, 30).unwrap();
        let mut out = Vec::new();
        LumaPacker::new(header, 128).pack_frame(&[255, 0, 255, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out, vec![0b1010_0000]);
    }

    #[test]
    fn the_threshold_selects_the_lit_value() {
        let header = PackHeader::new(8, 1, 1, 30).unwrap();
        let mut out = Vec::new();
        LumaPacker::new(header, 200).pack_frame(&[199, 200, 0, 0, 0, 0, 0, 0], &mut out);
        assert_eq!(out, vec![0b0100_0000]);
    }

    #[test]
    fn partial_rows_pad_with_unlit_bits() {
        let header = PackHeader::new(4, 1, 1, 30).unwrap();
        let mut out = Vec::new();
        LumaPacker::new(header, 128).pack_frame(&[255, 255, 255, 255], &mut out);
        assert_eq!(out, vec![0b1111_0000]);
    }

    #[test]
    fn each_row_starts_a_new_byte() {
        let header = PackHeader::new(4, 2, 1, 30).unwrap();
        let mut out = Vec::new();
        LumaPacker::new(header, 128).pack_frame(&[255, 0, 0, 0, 0, 0, 0, 255], &mut out);
        assert_eq!(out, vec![0b1000_0000, 0b0001_0000]);
        assert_eq!(out.len(), header.packed_frame_bytes());
    }
}
