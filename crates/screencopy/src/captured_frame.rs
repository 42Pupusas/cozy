//! Compositor pixel bytes normalised into encoder-ready RGBA8 scanlines.
//!
//! This is the mirror image of `bins/wallpaper`'s `wallpaper_pixels`: that
//! module turns decoded RGBA into compositor-facing `Xrgb8888`, this one turns
//! compositor-captured `Xrgb8888`/`Argb8888` back into the tightly packed,
//! unfiltered RGBA8 rows a PNG encoder wants.
//!
//! Three separate mismatches have to be reconciled here, and each is a real
//! screenshot bug if skipped:
//!
//! * **Byte order.** `wl_shm`'s `Xrgb8888`/`Argb8888` are *native-endian
//!   32-bit words* whose bytes on a little-endian machine land as B, G, R, A.
//!   PNG wants R, G, B, A. Copying straight through swaps red and blue, which
//!   looks plausible enough on greyscale UI to survive a careless eyeball.
//! * **Row padding.** The source steps by `stride`, the destination by
//!   `width * 4`; conflating them shears the image.
//! * **Row order.** `ScreencopyFrameFlags::YInvert` means the compositor
//!   handed us the image bottom-up.
//!
//! Deliberately holds no Wayland state: it takes a byte slice, so every case
//! below is provable without a compositor.

use crate::frame_params::FrameParams;
use platform::protocol::ScreencopyFrameFlags;

/// A finished capture: tightly packed RGBA8, top row first.
pub struct CapturedFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl CapturedFrame {
    /// Bytes per pixel in both the compositor's 32-bit formats and our output.
    const BYTES_PER_PIXEL: usize = 4;

    /// Convert one captured shm buffer into top-down RGBA8.
    ///
    /// `flags` is the raw `zwlr_screencopy_frame_v1.flags` bitmask; `source`
    /// must hold at least [`FrameParams::buffer_len`] bytes.
    ///
    /// # Errors
    ///
    /// [`CaptureError::ShortBuffer`] if `source` is smaller than the params
    /// promise -- reading past it would either panic or, worse, encode
    /// adjacent memory into an image file.
    pub fn from_shm(params: &FrameParams, flags: u32, source: &[u8]) -> Result<Self, CaptureError> {
        let needed = params.buffer_len();
        if source.len() < needed {
            return Err(CaptureError::ShortBuffer {
                needed,
                actual: source.len(),
            });
        }
        let width = params.width as usize;
        let row_bytes = width * Self::BYTES_PER_PIXEL;
        let height = params.height as usize;
        let y_invert = ScreencopyFrameFlags::YInvert.in_mask(flags);

        let mut rgba = vec![0u8; row_bytes * height];
        for row in 0..height {
            // On a y-inverted capture the last source row is the first image
            // row, so walk the source backwards while writing forwards.
            let source_row = if y_invert { height - 1 - row } else { row };
            let start = params.row_offset(source_row);
            let source_row_bytes = &source[start..start + row_bytes];
            let dest_row = &mut rgba[row * row_bytes..(row + 1) * row_bytes];
            Self::convert_row(params, source_row_bytes, dest_row);
        }
        Ok(Self {
            width: params.width,
            height: params.height,
            rgba,
        })
    }

    /// Rewrite one row of native-endian BGRA words as RGBA bytes.
    fn convert_row(params: &FrameParams, source: &[u8], dest: &mut [u8]) {
        let has_alpha = params.has_alpha();
        for (src, dst) in source
            .as_chunks::<{ Self::BYTES_PER_PIXEL }>()
            .0
            .iter()
            .zip(
                dest.as_chunks_mut::<{ Self::BYTES_PER_PIXEL }>()
                    .0
                    .iter_mut(),
            )
        {
            // Little-endian byte order within the 32-bit word: the low byte of
            // `0xAARRGGBB` is blue.
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            // `Xrgb8888`'s high byte is undefined padding. Compositors
            // routinely leave it zero, so copying it would encode a fully
            // transparent PNG of a perfectly good screenshot.
            dst[3] = if has_alpha { src[3] } else { 0xFF };
        }
    }

    /// Image width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Image height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// The tightly packed RGBA8 rows, top row first, with no filter bytes --
    /// exactly the layout `ixiptla-core`'s encoder expects.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// Why a captured buffer couldn't be interpreted.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// The mapped buffer is smaller than the advertised parameters require.
    ShortBuffer { needed: usize, actual: usize },
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShortBuffer { needed, actual } => write!(
                f,
                "captured buffer holds {actual} bytes but the frame needs {needed}"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

#[cfg(test)]
mod tests {
    use super::{CaptureError, CapturedFrame};
    use crate::frame_params::FrameParams;
    use platform::protocol::Format;

    /// One little-endian `0xAARRGGBB` word as it sits in shm memory.
    fn word(a: u8, r: u8, g: u8, b: u8) -> [u8; 4] {
        [b, g, r, a]
    }

    #[test]
    fn swaps_native_endian_bgra_words_into_rgba_byte_order() {
        // A distinctly red pixel: if the channel swap were skipped, this would
        // come back blue -- the classic silently-wrong screenshot.
        let params = FrameParams::new(Format::Argb8888.code(), 1, 1, 4).expect("valid");
        let source = word(0x80, 0xFF, 0x00, 0x00);
        let frame = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(frame.rgba(), &[0xFF, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn xrgb_padding_byte_becomes_opaque_rather_than_transparent() {
        // Compositors commonly leave Xrgb's high byte at zero. Copying it
        // through would make the whole screenshot fully transparent.
        let params = FrameParams::new(Format::Xrgb8888.code(), 1, 1, 4).expect("valid");
        let source = word(0x00, 0x12, 0x34, 0x56);
        let frame = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(frame.rgba(), &[0x12, 0x34, 0x56, 0xFF]);
    }

    #[test]
    fn argb_alpha_is_preserved_not_forced_opaque() {
        let params = FrameParams::new(Format::Argb8888.code(), 1, 1, 4).expect("valid");
        let source = word(0x00, 0x12, 0x34, 0x56);
        let frame = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(frame.rgba()[3], 0x00, "a real alpha channel must survive");
    }

    #[test]
    fn row_padding_is_skipped_rather_than_shifting_the_image() {
        // 2px wide, 16-byte stride: 8 bytes of junk padding per row. Reading
        // with width*4 would pull that junk in and shear every later row.
        let params = FrameParams::new(Format::Xrgb8888.code(), 2, 2, 16).expect("valid");
        let mut source = vec![0xEEu8; 32];
        source[0..4].copy_from_slice(&word(0, 1, 2, 3));
        source[4..8].copy_from_slice(&word(0, 4, 5, 6));
        source[16..20].copy_from_slice(&word(0, 7, 8, 9));
        source[20..24].copy_from_slice(&word(0, 10, 11, 12));
        let frame = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(
            frame.rgba(),
            &[
                1, 2, 3, 0xFF, 4, 5, 6, 0xFF, // row 0, padding skipped
                7, 8, 9, 0xFF, 10, 11, 12, 0xFF, // row 1
            ]
        );
    }

    #[test]
    fn y_invert_flag_flips_rows_so_screenshots_are_not_upside_down() {
        // wlroots GL captures set this; ignoring it yields upside-down images
        // on some compositors and not others.
        let params = FrameParams::new(Format::Xrgb8888.code(), 1, 2, 4).expect("valid");
        let mut source = Vec::new();
        source.extend_from_slice(&word(0, 0xAA, 0xAA, 0xAA)); // bottom of image
        source.extend_from_slice(&word(0, 0xBB, 0xBB, 0xBB)); // top of image
        let flags = platform::protocol::ScreencopyFrameFlags::YInvert as u32;
        let frame = CapturedFrame::from_shm(&params, flags, &source).expect("converts");
        assert_eq!(&frame.rgba()[0..4], &[0xBB, 0xBB, 0xBB, 0xFF]);
        assert_eq!(&frame.rgba()[4..8], &[0xAA, 0xAA, 0xAA, 0xFF]);

        // Without the flag the same bytes keep their original order.
        let upright = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(&upright.rgba()[0..4], &[0xAA, 0xAA, 0xAA, 0xFF]);
    }

    #[test]
    fn output_is_tightly_packed_at_the_advertised_dimensions() {
        let params = FrameParams::new(Format::Xrgb8888.code(), 3, 5, 32).expect("valid");
        let source = vec![0u8; params.buffer_len()];
        let frame = CapturedFrame::from_shm(&params, 0, &source).expect("converts");
        assert_eq!(frame.width(), 3);
        assert_eq!(frame.height(), 5);
        // Tight RGBA, not the padded source size -- what the encoder expects.
        assert_eq!(frame.rgba().len(), 3 * 5 * 4);
    }

    #[test]
    fn a_short_buffer_is_refused_instead_of_encoding_adjacent_memory() {
        let params = FrameParams::new(Format::Xrgb8888.code(), 4, 4, 16).expect("valid");
        let source = vec![0u8; 32];
        // Matched rather than compared with `assert_eq!`: giving
        // `CapturedFrame` a `PartialEq`/`Debug` derive purely for this
        // assertion would mean deep-comparing (and pretty-printing) a
        // multi-megabyte pixel buffer everywhere else.
        assert_eq!(
            CapturedFrame::from_shm(&params, 0, &source).err(),
            Some(CaptureError::ShortBuffer {
                needed: 64,
                actual: 32
            })
        );
    }
}
