//! A borrowed view over the compositor-written bytes of one capture.
//!
//! The bytes stay where the compositor put them (the session's shm mapping);
//! this type only knows how to read them. Converting to RGBA happens per
//! [`PixelWindow`], so cropping a frozen screen converts just the crop.
//!
//! Three mismatches are reconciled during that conversion, and each is a real
//! screenshot bug if skipped:
//!
//! * **Byte order.** `wl_shm`'s `Xrgb8888`/`Argb8888` are native-endian
//!   32-bit words whose bytes land as B, G, R, A; PNG wants R, G, B, A.
//! * **Row padding.** The source steps by `stride`, the destination by
//!   `width * 4`; conflating them shears the image.
//! * **Row order.** `ScreencopyFrameFlags::YInvert` means the compositor
//!   handed us the image bottom-up.

use platform::protocol::ScreencopyFrameFlags;

use crate::capture_region::CaptureRegion;
use crate::captured_frame::{CaptureError, CapturedFrame};
use crate::frame_params::FrameParams;
use crate::pixel_window::PixelWindow;

/// One captured buffer, read in place.
#[derive(Copy, Clone)]
pub struct FrameView<'a> {
    params: FrameParams,
    flags: u32,
    bytes: &'a [u8],
}

impl<'a> FrameView<'a> {
    const BYTES_PER_PIXEL: usize = 4;

    /// Wrap `bytes` as a frame laid out by `params`.
    ///
    /// # Errors
    ///
    /// [`CaptureError::ShortBuffer`] when `bytes` cannot hold the frame.
    pub const fn new(params: FrameParams, flags: u32, bytes: &'a [u8]) -> Result<Self, CaptureError> {
        let needed = params.buffer_len();
        if bytes.len() < needed {
            return Err(CaptureError::ShortBuffer {
                needed,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            params,
            flags,
            bytes,
        })
    }

    #[must_use]
    pub const fn params(&self) -> &FrameParams {
        &self.params
    }

    /// Whether the compositor stored the rows bottom-up.
    #[must_use]
    pub const fn is_y_inverted(&self) -> bool {
        ScreencopyFrameFlags::YInvert.in_mask(self.flags)
    }

    /// The whole frame as a window.
    #[must_use]
    pub const fn whole(&self) -> PixelWindow {
        PixelWindow::whole(self.params.width, self.params.height)
    }

    /// `region` in buffer pixels, or `None` if it is empty or leaves the frame.
    #[must_use]
    pub fn window(&self, region: CaptureRegion) -> Option<PixelWindow> {
        self.whole().sub(region)
    }

    /// Convert `window` (top-down image coordinates) into packed RGBA8.
    #[must_use]
    pub fn to_rgba(&self, window: PixelWindow) -> CapturedFrame {
        let x = window.x as usize;
        let width = window.width as usize;
        let height = window.height as usize;
        let frame_height = self.params.height as usize;
        let row_bytes = width * Self::BYTES_PER_PIXEL;
        let skip = x * Self::BYTES_PER_PIXEL;
        let mut rgba = vec![0u8; row_bytes * height];
        for (row, dest) in rgba.chunks_exact_mut(row_bytes).enumerate() {
            let image_row = window.y as usize + row;
            let source_row = if self.is_y_inverted() {
                frame_height - 1 - image_row
            } else {
                image_row
            };
            let start = self.params.row_offset(source_row) + skip;
            self.convert_row(&self.bytes[start..start + row_bytes], dest);
        }
        CapturedFrame::from_rgba(window.width, window.height, rgba)
    }

    /// Rewrite one row of native-endian BGRA words as RGBA bytes.
    fn convert_row(&self, source: &[u8], dest: &mut [u8]) {
        let has_alpha = self.params.has_alpha();
        let source = source.as_chunks::<4>().0;
        let dest = dest.as_chunks_mut::<4>().0;
        for (src, dst) in source.iter().zip(dest.iter_mut()) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = if has_alpha { src[3] } else { 0xFF };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FrameView;
    use crate::capture_region::CaptureRegion;
    use crate::captured_frame::{CaptureError, CapturedFrame};
    use crate::frame_params::FrameParams;
    use platform::protocol::{Format, ScreencopyFrameFlags};

    /// Converts whole synthetic buffers, the way a plain capture does.
    struct Whole;

    impl Whole {
        /// One little-endian `0xAARRGGBB` word as it sits in shm memory.
        const fn word(a: u8, r: u8, g: u8, b: u8) -> [u8; 4] {
            [b, g, r, a]
        }

        fn convert(
            format: Format,
            size: (u32, u32, u32),
            flags: u32,
            bytes: &[u8],
        ) -> Result<CapturedFrame, CaptureError> {
            let params = FrameParams::new(format.code(), size.0, size.1, size.2).expect("valid");
            let view = FrameView::new(params, flags, bytes)?;
            Ok(view.to_rgba(view.whole()))
        }
    }

    #[test]
    fn swaps_native_endian_bgra_words_into_rgba_byte_order() {
        let source = Whole::word(0x80, 0xFF, 0x00, 0x00);
        let frame = Whole::convert(Format::Argb8888, (1, 1, 4), 0, &source).expect("converts");
        assert_eq!(frame.rgba(), &[0xFF, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn xrgb_padding_byte_becomes_opaque_rather_than_transparent() {
        let source = Whole::word(0x00, 0x12, 0x34, 0x56);
        let frame = Whole::convert(Format::Xrgb8888, (1, 1, 4), 0, &source).expect("converts");
        assert_eq!(frame.rgba(), &[0x12, 0x34, 0x56, 0xFF]);
    }

    #[test]
    fn argb_alpha_is_preserved_not_forced_opaque() {
        let source = Whole::word(0x00, 0x12, 0x34, 0x56);
        let frame = Whole::convert(Format::Argb8888, (1, 1, 4), 0, &source).expect("converts");
        assert_eq!(frame.rgba()[3], 0x00);
    }

    #[test]
    fn row_padding_is_skipped_rather_than_shifting_the_image() {
        let mut source = vec![0xEEu8; 32];
        source[0..4].copy_from_slice(&Whole::word(0, 1, 2, 3));
        source[4..8].copy_from_slice(&Whole::word(0, 4, 5, 6));
        source[16..20].copy_from_slice(&Whole::word(0, 7, 8, 9));
        source[20..24].copy_from_slice(&Whole::word(0, 10, 11, 12));
        let frame = Whole::convert(Format::Xrgb8888, (2, 2, 16), 0, &source).expect("converts");
        assert_eq!(
            frame.rgba(),
            &[1, 2, 3, 0xFF, 4, 5, 6, 0xFF, 7, 8, 9, 0xFF, 10, 11, 12, 0xFF]
        );
    }

    #[test]
    fn y_invert_flag_flips_rows_so_screenshots_are_not_upside_down() {
        let mut source = Vec::new();
        source.extend_from_slice(&Whole::word(0, 0xAA, 0xAA, 0xAA));
        source.extend_from_slice(&Whole::word(0, 0xBB, 0xBB, 0xBB));
        let flags = ScreencopyFrameFlags::YInvert as u32;
        let frame = Whole::convert(Format::Xrgb8888, (1, 2, 4), flags, &source).expect("converts");
        assert_eq!(&frame.rgba()[0..4], &[0xBB, 0xBB, 0xBB, 0xFF]);
        assert_eq!(&frame.rgba()[4..8], &[0xAA, 0xAA, 0xAA, 0xFF]);

        let upright = Whole::convert(Format::Xrgb8888, (1, 2, 4), 0, &source).expect("converts");
        assert_eq!(&upright.rgba()[0..4], &[0xAA, 0xAA, 0xAA, 0xFF]);
    }

    #[test]
    fn output_is_tightly_packed_at_the_advertised_dimensions() {
        let source = vec![0u8; 32 * 5];
        let frame = Whole::convert(Format::Xrgb8888, (3, 5, 32), 0, &source).expect("converts");
        assert_eq!((frame.width(), frame.height()), (3, 5));
        assert_eq!(frame.rgba().len(), 3 * 5 * 4);
    }

    #[test]
    fn a_short_buffer_is_refused_instead_of_encoding_adjacent_memory() {
        let source = vec![0u8; 32];
        assert_eq!(
            Whole::convert(Format::Xrgb8888, (4, 4, 16), 0, &source).err(),
            Some(CaptureError::ShortBuffer {
                needed: 64,
                actual: 32
            })
        );
    }

    /// A 3x3 Xrgb frame with 4 bytes of row padding where pixel (x, y) has
    /// red = x and green = y.
    struct Grid {
        bytes: Vec<u8>,
        params: FrameParams,
    }

    impl Grid {
        fn new() -> Self {
            let params = FrameParams::new(Format::Xrgb8888.code(), 3, 3, 16).expect("valid");
            let mut bytes = vec![0xEE; params.buffer_len()];
            for y in 0..3u8 {
                for x in 0..3u8 {
                    let at = params.row_offset(usize::from(y)) + usize::from(x) * 4;
                    bytes[at..at + 4].copy_from_slice(&[0, y, x, 0]);
                }
            }
            Self { bytes, params }
        }

        fn view(&self, flags: u32) -> FrameView<'_> {
            FrameView::new(self.params, flags, &self.bytes).expect("sized")
        }

        const fn region(x: i32, y: i32, width: i32, height: i32) -> CaptureRegion {
            CaptureRegion {
                x,
                y,
                width,
                height,
            }
        }
    }

    #[test]
    fn a_crop_converts_only_the_selected_pixels() {
        let grid = Grid::new();
        let view = grid.view(0);
        let window = view.window(Grid::region(1, 1, 2, 2)).expect("inside");
        let crop = view.to_rgba(window);
        assert_eq!((crop.width(), crop.height()), (2, 2));
        assert_eq!(
            crop.rgba(),
            &[1, 1, 0, 0xFF, 2, 1, 0, 0xFF, 1, 2, 0, 0xFF, 2, 2, 0, 0xFF]
        );
    }

    #[test]
    fn a_crop_of_a_y_inverted_frame_is_taken_from_the_upright_image() {
        let grid = Grid::new();
        let view = grid.view(ScreencopyFrameFlags::YInvert as u32);
        let window = view.window(Grid::region(0, 0, 1, 1)).expect("inside");
        assert_eq!(view.to_rgba(window).rgba(), &[0, 2, 0, 0xFF]);
    }

    #[test]
    fn a_crop_outside_the_frame_is_refused() {
        let grid = Grid::new();
        assert!(grid.view(0).window(Grid::region(2, 0, 2, 1)).is_none());
    }

    #[test]
    fn the_view_reads_the_bytes_it_was_given_in_place() {
        let grid = Grid::new();
        let view = grid.view(0);
        assert!(std::ptr::eq(view.bytes, grid.bytes.as_slice()));
    }
}
