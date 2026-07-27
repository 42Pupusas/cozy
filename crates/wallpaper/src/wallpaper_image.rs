//! Decoding a wallpaper file into a flat RGBA8 buffer.
//!
//! This is the one place `bins/wallpaper` needs `ixiptla-core` at all --
//! everything past [`WallpaperImage::load`] (scaling, blitting into an
//! output-sized buffer) only ever touches plain `Vec<u8>` RGBA8 pixels, no
//! PNG-specific types. Keeping the decode call this narrow means a future
//! second format (JPEG, explicitly deferred -- see the PLAN.md scoping
//! note) only needs a second `load`-shaped function, not a rewrite of
//! [`crate::ScaleMode`].

use ixiptla_core::{DecodeError, ExpandError, Png, RgbaExpander};

/// A decoded wallpaper image: flat RGBA8 pixels (row-major, 4 bytes per
/// pixel, alpha last) plus its dimensions.
///
/// Deliberately *not* `ixiptla_core::Png` itself: `Png::pixels` is still in
/// whatever bit-depth/colour-type `IHDR` declared (indexed, sub-byte
/// grayscale, ...) -- [`RgbaExpander`] is what turns that into something
/// [`crate::ScaleMode`]'s blit math can index with plain `(x, y) * 4`
/// arithmetic, so expansion happens once here rather than in every scale
/// mode's blit loop.
#[derive(Debug, Clone)]
pub struct WallpaperImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl WallpaperImage {
    /// Decode `bytes` (a whole PNG file's contents) into RGBA8 pixels.
    ///
    /// `max_decompressed` bounds the inflated `IDAT` size (forwarded to
    /// [`Png::decode`] -- a decompression-bomb guard). A wallpaper file
    /// read straight off disk has no inherent size limit of its own, so
    /// callers should pick something generous relative to realistic
    /// monitor resolutions (e.g. `256 << 20` comfortably covers an 8K RGBA
    /// image) rather than the tiny bound a network-facing decoder would
    /// want.
    ///
    /// # Errors
    ///
    /// [`LoadError::Decode`] if the file isn't a valid PNG;
    /// [`LoadError::Expand`] if the decoded pixel data can't be turned into
    /// RGBA8 (e.g. a palette index pointing past the end of a truncated
    /// `PLTE`).
    pub fn load(bytes: &[u8], max_decompressed: usize) -> Result<Self, LoadError> {
        let png = Png::decode(bytes, max_decompressed)?;
        let expander =
            RgbaExpander::new(&png.ihdr, png.palette.as_ref(), png.transparency.as_ref());
        let rgba = expander.expand(&png.pixels)?;
        Ok(Self {
            width: png.ihdr.width,
            height: png.ihdr.height,
            rgba,
        })
    }
}

/// Why [`WallpaperImage::load`] failed.
#[derive(Debug)]
pub enum LoadError {
    Decode(DecodeError),
    Expand(ExpandError),
}

impl From<DecodeError> for LoadError {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl From<ExpandError> for LoadError {
    fn from(e: ExpandError) -> Self {
        Self::Expand(e)
    }
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "{e}"),
            Self::Expand(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadError {}

#[cfg(test)]
mod tests {
    use super::WallpaperImage;
    use ixiptla_core::Crc32;

    fn encode_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let crc = Crc32::new().update(&kind).update(data).finalize();
        out.extend_from_slice(&crc.to_be_bytes());
        out
    }

    /// A minimal 2x2, 8-bit truecolor (no alpha) PNG -- exercises the
    /// "alpha column ixiptla-core didn't produce" path through
    /// `RgbaExpander`, matching a real opaque wallpaper file.
    fn tiny_rgb_png() -> Vec<u8> {
        let mut file = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr_data = Vec::new();
        ihdr_data.extend_from_slice(&2u32.to_be_bytes());
        ihdr_data.extend_from_slice(&2u32.to_be_bytes());
        ihdr_data.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit truecolor, no interlace
        file.extend(encode_chunk(*b"IHDR", &ihdr_data));

        // 2 rows of 2 RGB pixels, filter byte 0 (None) each.
        let raw = [
            0u8, 255, 0, 0, 0, 255, 0, // row 0: red, green
            0, 0, 0, 255, 255, 255, 255, // row 1: blue, white
        ];
        let zlib_data = ixchel::zlib::compress(&raw);
        file.extend(encode_chunk(*b"IDAT", &zlib_data));
        file.extend(encode_chunk(*b"IEND", b""));
        file
    }

    #[test]
    fn loads_a_tiny_rgb_png_into_rgba8() {
        let png = tiny_rgb_png();
        let img = WallpaperImage::load(&png, 1 << 20).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(
            img.rgba,
            vec![
                255, 0, 0, 255, // red, opaque
                0, 255, 0, 255, // green, opaque
                0, 0, 255, 255, // blue, opaque
                255, 255, 255, 255, // white, opaque
            ]
        );
    }

    #[test]
    fn rejects_garbage_bytes() {
        let err = WallpaperImage::load(b"not a png", 1 << 20).unwrap_err();
        assert!(matches!(err, super::LoadError::Decode(_)));
    }
}
