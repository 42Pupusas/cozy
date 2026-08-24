//! Cell-grid rendering of a packed frame for the terminal path.
//!
//! Resolution comes from three sources, and this owner composes all of them:
//!
//! * A [`crate::SubcellGrid`] divides each cell into up to six blocks, so the
//!   sample grid is several times finer than the cell grid.
//! * A [`crate::CoverageSampler`] measures the lit area of each subcell
//!   instead of reading one source pixel, so nothing is discarded.
//! * A [`crate::DitherMatrix`] turns the resulting coverage back into two
//!   values without flattening every gradient into one hard edge.

use crate::coverage_sampler::CoverageSampler;
use crate::dither_matrix::DitherMatrix;
use crate::mono_frame::MonoFrame;
use crate::subcell_grid::SubcellGrid;

/// Converts packed frames into terminal cell rows.
pub struct AsciiRamp {
    columns: u32,
    rows: u32,
    subcells: SubcellGrid,
    sampler: CoverageSampler,
    dither: DitherMatrix,
    dithered: bool,
    threshold: u8,
}

impl AsciiRamp {
    /// Coverage at which a subcell lights when dithering is off.
    const FLAT_THRESHOLD: u8 = 128;

    /// Build a ramp reducing `source_width` by `source_height` frames onto a
    /// `columns` by `rows` cell grid.
    #[must_use]
    pub fn new(
        source_width: u32,
        source_height: u32,
        columns: u32,
        rows: u32,
        subcells: SubcellGrid,
    ) -> Self {
        Self {
            columns,
            rows,
            subcells,
            sampler: CoverageSampler::new(
                source_width,
                source_height,
                columns * subcells.columns(),
                rows * subcells.rows(),
            ),
            dither: DitherMatrix::new(),
            dithered: true,
            threshold: Self::FLAT_THRESHOLD,
        }
    }

    /// Turn ordered dithering on or off.
    #[must_use]
    pub const fn with_dither(mut self, dithered: bool) -> Self {
        self.dithered = dithered;
        self
    }

    /// The cell grid width.
    #[must_use]
    pub const fn columns(&self) -> u32 {
        self.columns
    }

    /// The cell grid height.
    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// The subcell division in use.
    #[must_use]
    pub const fn subcells(&self) -> SubcellGrid {
        self.subcells
    }

    /// The sample grid width in subcells.
    #[must_use]
    pub const fn sample_width(&self) -> u32 {
        self.sampler.width()
    }

    /// The sample grid height in subcells.
    #[must_use]
    pub const fn sample_height(&self) -> u32 {
        self.sampler.height()
    }

    /// Render `frame` into `out` as newline-separated rows.
    ///
    /// `out` is cleared and reused, so a playback loop performs no allocation
    /// after its first frame.
    pub fn render_into(&mut self, frame: &MonoFrame<'_>, out: &mut String) {
        out.clear();
        self.sampler.sample(frame);
        for row in 0..self.rows {
            for column in 0..self.columns {
                out.push(self.subcells.glyph(self.mask(column, row)));
            }
            if row + 1 < self.rows {
                out.push('\n');
            }
        }
    }

    /// The lit-subcell mask of the cell at `(column, row)`.
    fn mask(&self, column: u32, row: u32) -> u8 {
        let mut mask = 0u8;
        for subcell_row in 0..self.subcells.rows() {
            for subcell_column in 0..self.subcells.columns() {
                let x = column * self.subcells.columns() + subcell_column;
                let y = row * self.subcells.rows() + subcell_row;
                if self.is_lit(x, y) {
                    mask |= self.subcells.bit(subcell_column, subcell_row);
                }
            }
        }
        mask
    }

    /// Whether the subcell at `(x, y)` lights under the current policy.
    fn is_lit(&self, x: u32, y: u32) -> bool {
        let coverage = self.sampler.coverage(x, y);
        if self.dithered {
            self.dither.is_lit(x, y, coverage)
        } else {
            coverage >= self.threshold
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AsciiRamp, MonoFrame, SubcellGrid};
    use crate::pack_header::PackHeader;

    fn render(
        packed: &[u8],
        header: PackHeader,
        columns: u32,
        rows: u32,
        subcells: SubcellGrid,
    ) -> String {
        let mut ramp = AsciiRamp::new(header.width, header.height, columns, rows, subcells)
            .with_dither(false);
        let mut out = String::new();
        ramp.render_into(&MonoFrame::new(packed, header), &mut out);
        out
    }

    #[test]
    fn a_blank_frame_renders_only_spaces() {
        let header = PackHeader::new(4, 4, 1, 30).unwrap();
        let out = render(&[0u8; 4], header, 4, 2, SubcellGrid::Half);
        assert_eq!(out, "    \n    ");
    }

    #[test]
    fn a_full_frame_renders_only_full_blocks() {
        let header = PackHeader::new(4, 4, 1, 30).unwrap();
        let out = render(&[0xF0u8; 4], header, 4, 2, SubcellGrid::Half);
        assert_eq!(out, "\u{2588}\u{2588}\u{2588}\u{2588}\n\u{2588}\u{2588}\u{2588}\u{2588}");
    }

    #[test]
    fn half_blocks_encode_the_upper_and_lower_samples() {
        let header = PackHeader::new(4, 2, 1, 30).unwrap();
        let out = render(&[0xF0, 0x00], header, 4, 1, SubcellGrid::Half);
        assert_eq!(out, "\u{2580}\u{2580}\u{2580}\u{2580}");
    }

    #[test]
    fn quadrants_double_the_horizontal_resolution() {
        let header = PackHeader::new(2, 2, 1, 30).unwrap();
        let out = render(&[0b1000_0000, 0b0100_0000], header, 1, 1, SubcellGrid::Quadrant);
        assert_eq!(out, "\u{259A}");
    }

    #[test]
    fn sextants_triple_the_vertical_resolution() {
        let header = PackHeader::new(2, 3, 1, 30).unwrap();
        let packed = [0b1100_0000, 0b0000_0000, 0b1100_0000];
        let out = render(&packed, header, 1, 1, SubcellGrid::Sextant);
        assert_eq!(out.chars().count(), 1);
        assert_eq!(out.chars().next().unwrap(), SubcellGrid::Sextant.glyph(0b11_0011));
    }

    #[test]
    fn the_sample_grid_is_finer_than_the_cell_grid() {
        let ramp = AsciiRamp::new(480, 360, 100, 30, SubcellGrid::Sextant);
        assert_eq!(ramp.columns(), 100);
        assert_eq!(ramp.rows(), 30);
        assert_eq!(ramp.sample_width(), 200);
        assert_eq!(ramp.sample_height(), 90);
        assert_eq!(ramp.subcells(), SubcellGrid::Sextant);
    }

    #[test]
    fn rendering_reuses_the_destination_buffer() {
        let header = PackHeader::new(4, 4, 1, 30).unwrap();
        let mut ramp = AsciiRamp::new(4, 4, 2, 1, SubcellGrid::Half).with_dither(false);
        let mut out = String::from("stale");
        ramp.render_into(&MonoFrame::new(&[0u8; 4], header), &mut out);
        assert_eq!(out, "  ");
    }

    /// A frame at roughly a quarter coverage in every sample box.
    fn sparse_frame() -> ([u8; 8], PackHeader) {
        let mut packed = [0u8; 8];
        for row in (0..8).step_by(2) {
            packed[row] = 0b1000_1000;
        }
        (packed, PackHeader::new(8, 8, 1, 30).unwrap())
    }

    #[test]
    fn flat_thresholding_discards_a_faint_region() {
        let (packed, header) = sparse_frame();
        let out = render(&packed, header, 2, 1, SubcellGrid::Quadrant);
        assert_eq!(out, "  ");
    }

    #[test]
    fn dithering_keeps_the_faint_region_that_flat_thresholding_discards() {
        let (packed, header) = sparse_frame();
        let mut ramp = AsciiRamp::new(8, 8, 2, 1, SubcellGrid::Quadrant);
        let mut out = String::new();
        ramp.render_into(&MonoFrame::new(&packed, header), &mut out);
        assert!(
            out.chars().any(|glyph| glyph != ' '),
            "dither dropped a region flat thresholding also dropped"
        );
    }

    #[test]
    fn a_downscale_keeps_a_thin_feature_visible() {
        let header = PackHeader::new(64, 8, 1, 30).unwrap();
        let mut packed = vec![0u8; 64];
        for row in 0..8 {
            packed[row * 8] = 0b1000_0000;
        }
        let mut ramp = AsciiRamp::new(64, 8, 8, 2, SubcellGrid::Quadrant);
        let mut out = String::new();
        ramp.render_into(&MonoFrame::new(&packed, header), &mut out);
        assert!(out.chars().any(|c| c != ' ' && c != '\n'));
    }
}
