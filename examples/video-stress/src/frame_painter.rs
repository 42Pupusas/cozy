//! Expansion of one packed frame into an XRGB surface buffer.
//!
//! The painter walks the destination once and reads the source through a
//! per-axis index table. The tables depend only on the layout, so a resize
//! rebuilds them and every frame after that costs one lookup per pixel.

use crate::mono_frame::MonoFrame;
use crate::video_layout::VideoLayout;

/// Paints packed frames into a surface-sized XRGB buffer.
pub struct FramePainter {
    layout: VideoLayout,
    columns: Vec<Option<u32>>,
    rows: Vec<Option<u32>>,
    unlit: u32,
    lit: u32,
    pad: u32,
}

impl FramePainter {
    /// Build a painter for one layout and one clip size.
    #[must_use]
    pub fn new(
        layout: VideoLayout,
        source_width: u32,
        source_height: u32,
        unlit: u32,
        lit: u32,
        pad: u32,
    ) -> Self {
        Self {
            layout,
            columns: (0..layout.surface_width())
                .map(|x| layout.source_column(x, source_width))
                .collect(),
            rows: (0..layout.surface_height())
                .map(|y| layout.source_row(y, source_height))
                .collect(),
            unlit,
            lit,
            pad,
        }
    }

    /// The layout this painter was built for.
    #[must_use]
    pub const fn layout(&self) -> VideoLayout {
        self.layout
    }

    /// How many pixels one painted surface holds.
    #[must_use]
    pub const fn pixel_count(&self) -> usize {
        self.columns.len() * self.rows.len()
    }

    /// Whether this painter already targets a `width` by `height` surface.
    #[must_use]
    pub const fn covers(&self, width: u32, height: u32) -> bool {
        self.layout.surface_width() == width && self.layout.surface_height() == height
    }

    /// Paint `frame` across the whole surface, padding uncovered pixels.
    ///
    /// `out` must hold exactly [`Self::pixel_count`] pixels.
    pub fn paint(&self, frame: &MonoFrame<'_>, out: &mut [u32]) {
        assert_eq!(out.len(), self.pixel_count());
        let width = self.columns.len();
        for (y, out_row) in out.chunks_exact_mut(width).enumerate() {
            match self.rows.get(y).copied().flatten() {
                Some(source_y) => self.paint_row(frame, source_y, out_row),
                None => out_row.fill(self.pad),
            }
        }
    }

    /// Paint one covered surface row.
    fn paint_row(&self, frame: &MonoFrame<'_>, source_y: u32, out_row: &mut [u32]) {
        for (pixel, column) in out_row.iter_mut().zip(&self.columns) {
            *pixel = column.as_ref().map_or(self.pad, |source_x| {
                if frame.pixel(*source_x, source_y) {
                    self.lit
                } else {
                    self.unlit
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FramePainter, MonoFrame, VideoLayout};
    use crate::pack_header::PackHeader;
    use crate::video_layout::FitMode;

    const UNLIT: u32 = 0x0000_0000;
    const LIT: u32 = 0x00FF_FFFF;
    const PAD: u32 = 0x0012_3456;

    fn checker() -> ([u8; 2], PackHeader) {
        ([0b1010_0000, 0b0101_0000], PackHeader::new(4, 2, 1, 30).unwrap())
    }

    fn painter(surface_width: u32, surface_height: u32, mode: FitMode) -> FramePainter {
        let layout = VideoLayout::place(surface_width, surface_height, 4, 2, mode);
        FramePainter::new(layout, 4, 2, UNLIT, LIT, PAD)
    }

    #[test]
    fn a_matching_surface_reproduces_the_source() {
        let (packed, header) = checker();
        let painter = painter(4, 2, FitMode::Fit);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert_eq!(out[0..4], [LIT, UNLIT, LIT, UNLIT]);
        assert_eq!(out[4..8], [UNLIT, LIT, UNLIT, LIT]);
    }

    #[test]
    fn doubling_repeats_every_pixel() {
        let (packed, header) = checker();
        let painter = painter(8, 4, FitMode::Fit);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert_eq!(out[0..8], [LIT, LIT, UNLIT, UNLIT, LIT, LIT, UNLIT, UNLIT]);
        assert_eq!(out[8..16], out[0..8]);
    }

    #[test]
    fn uncovered_pixels_take_the_pad_color() {
        let (packed, header) = checker();
        let painter = painter(8, 8, FitMode::Fit);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert!(painter.layout().has_padding());
        assert_eq!(out[0], PAD);
        assert_eq!(out[out.len() - 1], PAD);
    }

    #[test]
    fn a_filled_surface_has_no_pad_pixels() {
        let (packed, header) = checker();
        let painter = painter(8, 8, FitMode::Fill);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert!(!out.contains(&PAD));
    }

    #[test]
    fn every_painted_pixel_is_one_of_the_three_colors() {
        let (packed, header) = checker();
        let painter = painter(37, 23, FitMode::Fit);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert!(
            out.iter()
                .all(|pixel| [UNLIT, LIT, PAD].contains(pixel))
        );
    }

    #[test]
    fn painting_twice_gives_the_same_pixels() {
        let (packed, header) = checker();
        let painter = painter(16, 9, FitMode::Fit);
        let frame = MonoFrame::new(&packed, header);
        let mut first = vec![0u32; painter.pixel_count()];
        let mut second = vec![7u32; painter.pixel_count()];
        painter.paint(&frame, &mut first);
        painter.paint(&frame, &mut second);
        assert_eq!(first, second);
    }

    #[test]
    fn the_pixel_count_matches_the_surface() {
        assert_eq!(painter(1920, 1080, FitMode::Fit).pixel_count(), 1920 * 1080);
    }

    #[test]
    fn a_painter_covers_only_the_size_it_was_built_for() {
        let painter = painter(1920, 1080, FitMode::Fit);
        assert!(painter.covers(1920, 1080));
        assert!(!painter.covers(1280, 720));
    }

    #[test]
    fn stretching_leaves_no_padding() {
        let (packed, header) = checker();
        let painter = painter(11, 7, FitMode::Stretch);
        let mut out = vec![0u32; painter.pixel_count()];
        painter.paint(&MonoFrame::new(&packed, header), &mut out);
        assert!(!out.contains(&PAD));
    }
}
