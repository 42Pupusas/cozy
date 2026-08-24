//! One packed frame borrowed from a [`crate::FramePack`].

use crate::pack_header::PackHeader;
use crate::unpack_table::UnpackTable;

/// A borrowed view of one frame's packed bits.
#[derive(Debug, Clone, Copy)]
pub struct MonoFrame<'pack> {
    packed: &'pack [u8],
    header: PackHeader,
}

impl<'pack> MonoFrame<'pack> {
    /// Wrap `packed`, which must hold exactly one frame for `header`.
    #[must_use]
    pub const fn new(packed: &'pack [u8], header: PackHeader) -> Self {
        Self { packed, header }
    }

    /// The pixel at `(x, y)`, where `true` is a lit pixel.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> bool {
        if x >= self.header.width || y >= self.header.height {
            return false;
        }
        let index = y as usize * self.header.row_bytes() + (x / 8) as usize;
        self.packed[index] >> (7 - x % 8) & 1 == 1
    }

    /// The geometry describing this frame.
    #[must_use]
    pub const fn header(&self) -> PackHeader {
        self.header
    }

    /// The raw packed bits of this frame.
    #[must_use]
    pub const fn packed(&self) -> &'pack [u8] {
        self.packed
    }

    /// Expand into `out` at the frame's own resolution, one `u32` per pixel.
    ///
    /// `out` must hold exactly [`PackHeader::pixel_count`] elements.
    pub fn expand_into(&self, table: &UnpackTable<u32>, out: &mut [u32]) {
        assert_eq!(out.len(), self.header.pixel_count());
        let row_bytes = self.header.row_bytes();
        let width = self.header.width as usize;
        for (packed_row, out_row) in self
            .packed
            .chunks_exact(row_bytes)
            .zip(out.chunks_exact_mut(width))
        {
            table.expand_row(packed_row, width, out_row);
        }
    }

    /// Expand into `out`, scaling to `width` by `height` with a nearest
    /// neighbour sample and no allocation.
    ///
    /// Nearest neighbour is deliberate: the source is two-valued, so any
    /// filtered resample would invent grey pixels and change what the
    /// renderer is asked to push.
    pub fn scale_into(&self, table: &UnpackTable<u32>, width: u32, height: u32, out: &mut [u32]) {
        assert_eq!(out.len(), width as usize * height as usize);
        if width == 0 || height == 0 {
            return;
        }
        let row_bytes = self.header.row_bytes();
        let source_width = self.header.width as usize;
        let mut scratch = vec![0u32; source_width];
        let mut last_source_row = usize::MAX;
        for (y, out_row) in out.chunks_exact_mut(width as usize).enumerate() {
            let source_y = Self::sample(u32::try_from(y).unwrap_or(u32::MAX), height, self.header.height);
            if source_y != last_source_row {
                let start = source_y * row_bytes;
                table.expand_row(
                    &self.packed[start..start + row_bytes],
                    source_width,
                    &mut scratch,
                );
                last_source_row = source_y;
            }
            for (x, pixel) in out_row.iter_mut().enumerate() {
                *pixel =
                    scratch[Self::sample(u32::try_from(x).unwrap_or(u32::MAX), width, self.header.width)];
            }
        }
    }

    /// Map a destination coordinate onto its nearest source coordinate.
    fn sample(destination: u32, destination_extent: u32, source_extent: u32) -> usize {
        let scaled = u64::from(destination) * u64::from(source_extent);
        usize::try_from(scaled / u64::from(destination_extent)).unwrap_or(usize::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{MonoFrame, PackHeader, UnpackTable};

    fn frame() -> ([u8; 2], PackHeader) {
        ([0b1010_0000, 0b0101_0000], PackHeader::new(4, 2, 1, 30).unwrap())
    }

    #[test]
    fn reads_individual_pixels() {
        let (packed, header) = frame();
        let frame = MonoFrame::new(&packed, header);
        assert!(frame.pixel(0, 0));
        assert!(!frame.pixel(1, 0));
        assert!(!frame.pixel(0, 1));
        assert!(frame.pixel(1, 1));
    }

    #[test]
    fn out_of_range_pixels_read_as_unlit() {
        let (packed, header) = frame();
        let frame = MonoFrame::new(&packed, header);
        assert!(!frame.pixel(4, 0));
        assert!(!frame.pixel(0, 2));
    }

    #[test]
    fn expands_at_native_resolution() {
        let (packed, header) = frame();
        let table = UnpackTable::new(0u32, 0x00FF_FFFF);
        let mut out = [0u32; 8];
        MonoFrame::new(&packed, header).expand_into(&table, &mut out);
        assert_eq!(
            out,
            [0x00FF_FFFF, 0, 0x00FF_FFFF, 0, 0, 0x00FF_FFFF, 0, 0x00FF_FFFF]
        );
    }

    #[test]
    fn doubling_repeats_every_pixel_and_row() {
        let (packed, header) = frame();
        let table = UnpackTable::new(0u32, 1u32);
        let mut out = [0u32; 32];
        MonoFrame::new(&packed, header).scale_into(&table, 8, 4, &mut out);
        assert_eq!(out[0..8], [1, 1, 0, 0, 1, 1, 0, 0]);
        assert_eq!(out[8..16], [1, 1, 0, 0, 1, 1, 0, 0]);
        assert_eq!(out[16..24], [0, 0, 1, 1, 0, 0, 1, 1]);
    }

    #[test]
    fn downscaling_keeps_the_two_source_values() {
        let (packed, header) = frame();
        let table = UnpackTable::new(0u32, 1u32);
        let mut out = [9u32; 2];
        MonoFrame::new(&packed, header).scale_into(&table, 2, 1, &mut out);
        assert!(out.iter().all(|pixel| *pixel == 0 || *pixel == 1));
    }
}
