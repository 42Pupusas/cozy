//! An owned, encoder-ready capture: tightly packed RGBA8, top row first.
//!
//! Produced by [`crate::FrameView::to_rgba`], which does the byte-order,
//! row-padding and row-order normalisation while reading the compositor's
//! buffer in place.

/// A finished capture: tightly packed RGBA8, top row first.
pub struct CapturedFrame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl CapturedFrame {
    /// Take ownership of already converted, tightly packed RGBA8 rows.
    pub(crate) const fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self {
            width,
            height,
            rgba,
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
