//! The buffer geometry a compositor demands for one capture.
//!
//! Unlike our own surfaces -- where *we* pick `Xrgb8888` and compute a tight
//! `width * 4` stride -- `zwlr_screencopy_frame_v1.buffer` is the compositor
//! telling *us* what to allocate. It may name a format we can't interpret and
//! a stride with row padding, so the values need validating once, at the
//! boundary, rather than being trusted piecemeal at each use.

use platform::protocol::Format;

/// Validated `buffer` event parameters: what to allocate, and how to read it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FrameParams {
    /// The pixel layout the compositor will write.
    pub format: Format,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Bytes per row, which may exceed `width * 4` when the compositor pads
    /// rows for alignment. Reading with `width * 4` instead would shear the
    /// image progressively -- each row drifting further left -- so every
    /// row-walk must step by this value.
    pub stride: u32,
    /// `stride * height`, proven to fit `usize` by [`Self::new`] so callers
    /// can allocate and index without a fallible conversion.
    buffer_len: usize,
}

impl FrameParams {
    /// Validate one `buffer` event's arguments.
    ///
    /// # Errors
    ///
    /// [`ParamsError::UnsupportedFormat`] when the compositor names a layout
    /// outside the two 32-bit orderings we can decode (it may advertise
    /// `RGB565`, `XBGR8888`, ...), [`ParamsError::ZeroSized`] for a degenerate
    /// image, and [`ParamsError::StrideTooSmall`] when the stride can't even
    /// hold one row -- all three would otherwise surface much later as
    /// misread bytes rather than a clean failure.
    pub fn new(format: u32, width: u32, height: u32, stride: u32) -> Result<Self, ParamsError> {
        let format = Format::from_code(format).ok_or(ParamsError::UnsupportedFormat(format))?;
        if width == 0 || height == 0 {
            return Err(ParamsError::ZeroSized { width, height });
        }
        // `width * 4` can't overflow a u64, so widen rather than risk a u32
        // multiply wrapping into a stride that looks acceptable.
        let minimum = u64::from(width) * 4;
        if u64::from(stride) < minimum {
            return Err(ParamsError::StrideTooSmall { stride, width });
        }
        // Prove the whole buffer is addressable *here*, so every later size
        // and offset is a plain `usize` the caller can index with. Two u32s
        // multiply to as much as ~2^64, which genuinely doesn't fit a 32-bit
        // `usize`; checking once at the boundary beats a fallible conversion
        // (or a silently truncating cast) at each use.
        let bytes = u64::from(stride) * u64::from(height);
        let buffer_len = usize::try_from(bytes).map_err(|_| ParamsError::TooLarge { bytes })?;
        Ok(Self {
            format,
            width,
            height,
            stride,
            buffer_len,
        })
    }

    /// Total bytes the compositor will write: `stride * height`.
    ///
    /// Uses the padded stride, not `width * 4`: allocating the tight size for
    /// a padded frame means the final row runs off the end of the mapping.
    #[must_use]
    pub const fn buffer_len(&self) -> usize {
        self.buffer_len
    }

    /// Byte offset of row `y` within the captured buffer.
    ///
    /// Takes a `usize` because callers reach it while walking rows with a
    /// plain index; for any `y < height` the result stays within
    /// [`Self::buffer_len`], so no further checking is needed at the call site.
    #[must_use]
    pub const fn row_offset(&self, y: usize) -> usize {
        // Cannot exceed `buffer_len`, which `new` proved fits in `usize`.
        self.stride as usize * y
    }

    /// Whether the source pixels carry meaningful alpha.
    ///
    /// `Xrgb8888`'s top byte is undefined padding, so it must be replaced with
    /// opaque rather than copied -- some compositors leave it zero, which
    /// would encode a fully transparent PNG of a perfectly good screenshot.
    #[must_use]
    pub const fn has_alpha(&self) -> bool {
        matches!(self.format, Format::Argb8888)
    }
}

/// Why a compositor's advertised buffer parameters were rejected.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ParamsError {
    /// A `wl_shm` format code outside the two we can decode.
    UnsupportedFormat(u32),
    /// A zero width or height.
    ZeroSized { width: u32, height: u32 },
    /// A stride that can't hold `width` 32-bit pixels.
    StrideTooSmall { stride: u32, width: u32 },
    /// The frame needs more bytes than this machine can address.
    TooLarge { bytes: u64 },
}

impl std::fmt::Display for ParamsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormat(code) => {
                write!(f, "compositor offered unsupported buffer format {code}")
            }
            Self::ZeroSized { width, height } => {
                write!(f, "compositor offered a {width}x{height} buffer")
            }
            Self::StrideTooSmall { stride, width } => {
                write!(f, "stride {stride} is too small for {width} 32-bit pixels")
            }
            Self::TooLarge { bytes } => {
                write!(
                    f,
                    "frame needs {bytes} bytes, more than this machine can address"
                )
            }
        }
    }
}

impl std::error::Error for ParamsError {}

#[cfg(test)]
mod tests {
    use super::{FrameParams, ParamsError};
    use platform::protocol::Format;

    #[test]
    fn accepts_both_decodable_formats() {
        let argb = FrameParams::new(Format::Argb8888.code(), 4, 3, 16).expect("argb is decodable");
        assert_eq!(argb.format, Format::Argb8888);
        assert!(argb.has_alpha());
        let xrgb = FrameParams::new(Format::Xrgb8888.code(), 4, 3, 16).expect("xrgb is decodable");
        assert_eq!(xrgb.format, Format::Xrgb8888);
        // The distinction matters: Xrgb's top byte is padding, so treating it
        // as alpha can yield a fully transparent capture.
        assert!(!xrgb.has_alpha());
    }

    #[test]
    fn rejects_a_format_we_cannot_interpret() {
        // wl_shm advertises many formats beyond the two 32-bit orderings we
        // handle; misreading one silently produces a garbled screenshot.
        assert_eq!(
            FrameParams::new(9999, 4, 3, 16),
            Err(ParamsError::UnsupportedFormat(9999))
        );
    }

    #[test]
    fn rejects_degenerate_dimensions() {
        assert_eq!(
            FrameParams::new(Format::Xrgb8888.code(), 0, 3, 16),
            Err(ParamsError::ZeroSized {
                width: 0,
                height: 3
            })
        );
        assert_eq!(
            FrameParams::new(Format::Xrgb8888.code(), 4, 0, 16),
            Err(ParamsError::ZeroSized {
                width: 4,
                height: 0
            })
        );
    }

    #[test]
    fn rejects_a_stride_that_cannot_hold_a_row() {
        assert_eq!(
            FrameParams::new(Format::Xrgb8888.code(), 4, 3, 15),
            Err(ParamsError::StrideTooSmall {
                stride: 15,
                width: 4
            })
        );
        // Exactly tight is fine -- padding is optional, not mandatory.
        assert!(FrameParams::new(Format::Xrgb8888.code(), 4, 3, 16).is_ok());
    }

    #[test]
    fn a_huge_width_cannot_wrap_into_an_acceptable_stride() {
        // width * 4 overflows u32 here; a 32-bit multiply would wrap to 0 and
        // make any stride look sufficient, so the check widens to u64 first.
        let huge = u32::MAX / 2;
        assert_eq!(
            FrameParams::new(Format::Xrgb8888.code(), huge, 1, 64),
            Err(ParamsError::StrideTooSmall {
                stride: 64,
                width: huge
            })
        );
    }

    #[test]
    #[cfg_attr(
        target_pointer_width = "64",
        ignore = "u32 stride * u32 height always fits a 64-bit usize; only reachable on 32-bit"
    )]
    fn refuses_a_frame_larger_than_the_address_space() {
        assert!(matches!(
            FrameParams::new(Format::Xrgb8888.code(), 4, u32::MAX, u32::MAX),
            Err(ParamsError::TooLarge { .. })
        ));
    }

    #[test]
    fn sizes_and_offsets_use_the_padded_stride() {
        // 4px wide but 24-byte stride: 8 bytes of row padding. Using width*4
        // would under-allocate and shear the image as rows drift.
        let p = FrameParams::new(Format::Xrgb8888.code(), 4, 3, 24).expect("valid");
        assert_eq!(p.buffer_len(), 72);
        assert_eq!(p.row_offset(0), 0);
        assert_eq!(p.row_offset(1), 24);
        assert_eq!(p.row_offset(2), 48);
    }
}
