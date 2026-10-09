//! Image loading and pixel-format conversion for wallpaper layer surfaces.
//!
//! Wayland shm in this application is deliberately opaque `Xrgb8888`. This
//! module is the single boundary between the headless RGBA decoder/scaler and
//! that compositor-facing format, so alpha handling and byte packing cannot
//! diverge between outputs.

use std::path::{Path, PathBuf};

use wallpaper::{LoadError, ScaleMode, WallpaperImage};

use crate::appearance::{Appearance, BackgroundSource};

/// Generous decoder bound: enough for a 16K RGBA image while still rejecting
/// compressed inputs that expand without a realistic desktop-use limit.
const MAX_DECOMPRESSED_BYTES: usize = 1 << 30;

/// Where a wallpaper comes from and how it fits an output.
///
/// The decoded image is deliberately not kept: it is as large as a full
/// frame, and it is only needed when a surface is configured. Each render
/// decodes it, packs it into the shm buffer, and frees it again.
#[derive(Clone)]
pub struct WallpaperPixels {
    path: PathBuf,
    mode: ScaleMode,
}

impl WallpaperPixels {
    /// The wallpaper `appearance` references, or `None` for a solid color.
    #[must_use]
    pub fn from_appearance(appearance: &Appearance) -> Option<Self> {
        match &appearance.source {
            BackgroundSource::Image(path) => Some(Self {
                path: path.clone(),
                mode: appearance.mode,
            }),
            BackgroundSource::SolidColor => None,
        }
    }

    fn decode(path: &Path) -> Result<WallpaperImage, PixelLoadError> {
        let bytes = std::fs::read(path).map_err(PixelLoadError::Read)?;
        WallpaperImage::load(&bytes, MAX_DECOMPRESSED_BYTES).map_err(PixelLoadError::Decode)
    }

    /// Decode the image and render it into caller-owned XRGB storage.
    pub fn render_xrgb_into(
        &self,
        width: u32,
        height: u32,
        pad: [u8; 4],
        out: &mut [u32],
    ) -> Result<(), PixelLoadError> {
        assert_eq!(out.len(), width as usize * height as usize);
        let image = Self::decode(&self.path)?;
        let rgba = self.mode.blit(&image, width, height, pad);
        drop(image);
        for (destination, pixel) in out.iter_mut().zip(rgba.as_chunks::<4>().0.iter()) {
            *destination = Self::pack_xrgb(pixel);
        }
        drop(rgba);
        platform::sys::Heap::release_free_memory();
        Ok(())
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
    use std::path::PathBuf;

    use wallpaper::ScaleMode;

    use super::{PixelLoadError, WallpaperPixels};

    #[test]
    fn a_missing_file_fails_at_render_time_and_leaves_the_buffer_alone() {
        let wallpaper = WallpaperPixels {
            path: PathBuf::from("/nonexistent/wallpaper.png"),
            mode: ScaleMode::Center,
        };
        let mut out = [7u32; 4];
        let result = wallpaper.render_xrgb_into(2, 2, [0; 4], &mut out);
        assert!(matches!(result, Err(PixelLoadError::Read(_))));
        assert_eq!(out, [7; 4]);
    }

    #[test]
    fn transparent_pixels_are_composited_over_black_for_xrgb() {
        assert_eq!(
            WallpaperPixels::pack_xrgb(&[200, 100, 50, 128]),
            0x00_64_32_19
        );
    }
}
