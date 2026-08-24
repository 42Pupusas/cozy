//! Area-averaged reduction of a packed frame onto a sample grid.
//!
//! Point sampling throws away every source pixel it does not land on, so a
//! 480 pixel wide frame shown on 200 samples discards more than half the
//! picture and makes thin features flicker. This owner instead measures what
//! fraction of each destination box is lit, which keeps a feature visible as
//! partial coverage even when it is thinner than one box.
//!
//! The sample loop walks the source in row order and accumulates into the
//! destination, so its cost tracks the source pixel count, not the product of
//! the two resolutions.

use crate::mono_frame::MonoFrame;
use crate::unpack_table::UnpackTable;

/// A half-open source range covering one destination sample.
type Span = (u32, u32);

/// Reduces packed frames onto a fixed sample grid by area coverage.
pub struct CoverageSampler {
    width: u32,
    height: u32,
    column_spans: Vec<Span>,
    row_spans: Vec<Span>,
    lit: Vec<u32>,
    scratch: Vec<u8>,
    table: UnpackTable<u8>,
}

impl CoverageSampler {
    /// Build a sampler reducing `source_width` by `source_height` frames onto
    /// a `width` by `height` sample grid.
    #[must_use]
    pub fn new(source_width: u32, source_height: u32, width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            column_spans: Self::spans(source_width, width),
            row_spans: Self::spans(source_height, height),
            lit: vec![0; width as usize * height as usize],
            scratch: vec![0; source_width as usize],
            table: UnpackTable::new(0, 1),
        }
    }

    /// The sample grid width.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// The sample grid height.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Measure `frame` onto the sample grid.
    pub fn sample(&mut self, frame: &MonoFrame<'_>) {
        let header = frame.header();
        let source_width = header.width as usize;
        let row_bytes = header.row_bytes();
        for row in 0..self.height as usize {
            let (row_start, row_end) = self.row_spans[row];
            let base = row * self.width as usize;
            self.lit[base..base + self.width as usize].fill(0);
            for source_row in row_start..row_end.min(header.height) {
                let start = source_row as usize * row_bytes;
                self.table.expand_row(
                    &frame.packed()[start..start + row_bytes],
                    source_width,
                    &mut self.scratch,
                );
                for (column, span) in self.column_spans.iter().enumerate() {
                    let (from, to) = (span.0 as usize, (span.1 as usize).min(source_width));
                    self.lit[base + column] +=
                        self.scratch[from..to].iter().copied().map(u32::from).sum::<u32>();
                }
            }
        }
    }

    /// The lit fraction of the sample at `(column, row)`, from 0 to 255.
    #[must_use]
    pub fn coverage(&self, column: u32, row: u32) -> u8 {
        if column >= self.width || row >= self.height {
            return 0;
        }
        let index = row as usize * self.width as usize + column as usize;
        let area = self.area(column, row);
        if area == 0 {
            return 0;
        }
        u8::try_from(self.lit[index] * 255 / area).unwrap_or(u8::MAX)
    }

    /// Whether the sample at `(column, row)` is lit at `threshold` coverage.
    #[must_use]
    pub fn is_lit(&self, column: u32, row: u32, threshold: u8) -> bool {
        self.coverage(column, row) >= threshold
    }

    /// Source pixels contributing to the sample at `(column, row)`.
    fn area(&self, column: u32, row: u32) -> u32 {
        let (column_from, column_to) = self.column_spans[column as usize];
        let (row_from, row_to) = self.row_spans[row as usize];
        (column_to - column_from) * (row_to - row_from)
    }

    /// Divide `source_extent` into `destination_extent` half-open spans, each
    /// holding at least one source coordinate.
    fn spans(source_extent: u32, destination_extent: u32) -> Vec<Span> {
        (0..destination_extent)
            .map(|index| {
                let from = Self::edge(index, destination_extent, source_extent);
                let to = Self::edge(index + 1, destination_extent, source_extent);
                (from, to.max(from + 1))
            })
            .collect()
    }

    /// The source coordinate where destination sample `index` begins.
    fn edge(index: u32, destination_extent: u32, source_extent: u32) -> u32 {
        if destination_extent == 0 {
            return 0;
        }
        let scaled = u64::from(index) * u64::from(source_extent);
        u32::try_from(scaled / u64::from(destination_extent)).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{CoverageSampler, MonoFrame};
    use crate::pack_header::PackHeader;

    #[test]
    fn a_fully_lit_frame_covers_every_sample() {
        let header = PackHeader::new(8, 4, 1, 30).unwrap();
        let packed = [0xFFu8; 4];
        let mut sampler = CoverageSampler::new(8, 4, 4, 2);
        sampler.sample(&MonoFrame::new(&packed, header));
        for row in 0..2 {
            for column in 0..4 {
                assert_eq!(sampler.coverage(column, row), 255);
            }
        }
    }

    #[test]
    fn a_blank_frame_covers_nothing() {
        let header = PackHeader::new(8, 4, 1, 30).unwrap();
        let packed = [0u8; 4];
        let mut sampler = CoverageSampler::new(8, 4, 4, 2);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert_eq!(sampler.coverage(0, 0), 0);
        assert!(!sampler.is_lit(0, 0, 1));
    }

    #[test]
    fn a_half_lit_box_reports_half_coverage() {
        let header = PackHeader::new(8, 2, 1, 30).unwrap();
        let packed = [0b1010_1010, 0b1010_1010];
        let mut sampler = CoverageSampler::new(8, 2, 2, 1);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert_eq!(sampler.coverage(0, 0), 127);
        assert_eq!(sampler.coverage(1, 0), 127);
    }

    #[test]
    fn a_feature_thinner_than_a_box_still_registers() {
        let header = PackHeader::new(8, 1, 1, 30).unwrap();
        let packed = [0b0000_0001];
        let mut sampler = CoverageSampler::new(8, 1, 2, 1);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert_eq!(sampler.coverage(0, 0), 0);
        assert!(sampler.coverage(1, 0) > 0);
    }

    #[test]
    fn upsampling_gives_every_sample_one_source_pixel() {
        let header = PackHeader::new(2, 1, 1, 30).unwrap();
        let packed = [0b1000_0000];
        let mut sampler = CoverageSampler::new(2, 1, 4, 1);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert_eq!(sampler.coverage(0, 0), 255);
        assert_eq!(sampler.coverage(1, 0), 255);
        assert_eq!(sampler.coverage(2, 0), 0);
        assert_eq!(sampler.coverage(3, 0), 0);
    }

    #[test]
    fn out_of_range_samples_read_as_unlit() {
        let header = PackHeader::new(8, 4, 1, 30).unwrap();
        let packed = [0xFFu8; 4];
        let mut sampler = CoverageSampler::new(8, 4, 4, 2);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert_eq!(sampler.coverage(4, 0), 0);
        assert_eq!(sampler.coverage(0, 2), 0);
        assert_eq!((sampler.width(), sampler.height()), (4, 2));
    }

    #[test]
    fn the_threshold_selects_which_samples_light() {
        let header = PackHeader::new(8, 2, 1, 30).unwrap();
        let packed = [0b1010_1010, 0b1010_1010];
        let mut sampler = CoverageSampler::new(8, 2, 2, 1);
        sampler.sample(&MonoFrame::new(&packed, header));
        assert!(sampler.is_lit(0, 0, 100));
        assert!(!sampler.is_lit(0, 0, 200));
    }

    #[test]
    fn sampling_twice_does_not_accumulate() {
        let header = PackHeader::new(8, 2, 1, 30).unwrap();
        let packed = [0xFFu8, 0xFF];
        let mut sampler = CoverageSampler::new(8, 2, 2, 1);
        let frame = MonoFrame::new(&packed, header);
        sampler.sample(&frame);
        sampler.sample(&frame);
        assert_eq!(sampler.coverage(0, 0), 255);
    }
}
