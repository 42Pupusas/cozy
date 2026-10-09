//! Image loading and pixel-format conversion for wallpaper layer surfaces.
//!
//! Wayland shm in this application is deliberately opaque `Xrgb8888`. This
//! module is the single boundary between the headless RGBA decoder/scaler and
//! that compositor-facing format, so alpha handling and byte packing cannot
//! diverge between outputs.

use std::{cell::RefCell, path::Path};

use wallpaper::{LoadError, ScaleMode, WallpaperImage};

use crate::appearance::{Appearance, BackgroundSource};

/// Generous decoder bound: enough for a 16K RGBA image while still rejecting
/// compressed inputs that expand without a realistic desktop-use limit.
const MAX_DECOMPRESSED_BYTES: usize = 1 << 30;

/// A decoded image plus the fitting policy used for every output.
#[derive(Clone)]
pub struct WallpaperPixels {
    image: WallpaperImage,
    mode: ScaleMode,
    /// Scratch RGBA storage is kept behind interior mutability because a
    /// wallpaper is shared immutably while rendering replacement surfaces.
    /// Reusing it removes a frame-sized allocation from every render.
    rgba_scratch: RefCell<Vec<u8>>,
}

impl WallpaperPixels {
    /// Read and decode one PNG wallpaper from `path`.
    pub fn load(path: &Path, mode: ScaleMode) -> Result<Self, PixelLoadError> {
        let bytes = std::fs::read(path).map_err(PixelLoadError::Read)?;
        let image =
            WallpaperImage::load(&bytes, MAX_DECOMPRESSED_BYTES).map_err(PixelLoadError::Decode)?;
        Ok(Self {
            image,
            mode,
            rgba_scratch: RefCell::new(Vec::new()),
        })
    }

    /// Build image pixels when `appearance` references an image. Solid-color
    /// rules deliberately avoid decoding a file at all.
    pub fn from_appearance(appearance: &Appearance) -> Result<Option<Self>, PixelLoadError> {
        match &appearance.source {
            BackgroundSource::Image(path) => Self::load(path, appearance.mode).map(Some),
            BackgroundSource::SolidColor => Ok(None),
        }
    }

    /// Render into caller-owned XRGB storage so repeated output redraws do not
    /// allocate a new pixel vector every frame.
    pub fn render_xrgb_into(&self, width: u32, height: u32, pad: [u8; 4], out: &mut [u32]) {
        assert_eq!(out.len(), width as usize * height as usize);
        let mut rgba = self.rgba_scratch.borrow_mut();
        rgba.resize(out.len() * 4, 0);
        self.mode
            .blit_into(&self.image, width, height, pad, &mut rgba);
        for (destination, pixel) in out.iter_mut().zip(rgba.as_chunks::<4>().0.iter()) {
            *destination = Self::pack_xrgb(pixel);
        }
    }

    /// Convert straight-alpha RGBA to opaque XRGB by compositing over black.
    /// The protocol format does not carry alpha, so dropping it would make
    /// transparent PNG pixels show their RGB channels unexpectedly.
    #[must_use]
    fn pack_xrgb(rgba: &[u8]) -> u32 {
        let alpha = u32::from(rgba[3]);
        let red = u32::from(rgba[0]) * alpha / 255;
        let green = u32::from(rgba[1]) * alpha / 255;
        let blue = u32::from(rgba[2]) * alpha / 255;
        (red << 16) | (green << 8) | blue
    }
}

/// Failure while obtaining the one immutable source image shared by outputs.
#[derive(Debug)]
pub enum PixelLoadError {
    Read(std::io::Error),
    Decode(LoadError),
}

impl core::fmt::Display for PixelLoadError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "failed to read wallpaper: {error}"),
            Self::Decode(error) => write!(formatter, "failed to decode wallpaper PNG: {error}"),
        }
    }
}

impl std::error::Error for PixelLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::Decode(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WallpaperPixels;

    #[test]
    fn transparent_pixels_are_composited_over_black_for_xrgb() {
        assert_eq!(
            WallpaperPixels::pack_xrgb(&[200, 100, 50, 128]),
            0x00_64_32_19
        );
    }
}
