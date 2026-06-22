//! `font` — zero-dependency TTF/OTF rasterizer for the terminal.
//!
//! Pipeline:
//! 1. [`parse`]   — read raw TTF/OTF bytes, locate tables
//! 2. [`tables`]  — decode `head`, `hhea`, `cmap`, `hmtx`, `loca`, `glyf`
//! 3. [`outline`] — walk `glyf` contours into [`Outline`] (lines + curves)
//! 4. [`raster`]  — scanline rasterize an [`Outline`] into an 8-bit alpha bitmap
//! 5. [`atlas`]   — pack rendered glyphs into a 2-D texture atlas
//! 6. [`face`]    — public [`FontFace`] API tying everything together

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    dead_code,          // many table fields reserved for later use
    unused_variables,   // scale/ascender used in future raster integration
)]

mod atlas;
mod outline;
mod parse;
mod raster;
mod tables;

pub mod face;
mod tests;

pub use atlas::{Atlas, GlyphEntry};
pub use face::{FontFace, GlyphMetrics, RasterizedGlyph};
pub use raster::Bitmap;
