//! `font` — zero-dependency TTF/OTF rasterizer for the terminal.
//!
//! Pipeline:
//! 1. [`parse`]   — read raw TTF/OTF bytes, locate tables
//! 2. [`tables`]  — decode `head`, `hhea`, `cmap`, `hmtx`, `loca`, `glyf`
//! 3. [`outline`] — walk `glyf` contours into [`Outline`] (lines + curves)
//! 4. [`raster`]  — scanline rasterize an [`Outline`] into an 8-bit alpha bitmap
//! 5. [`atlas`]   — pack rendered glyphs into a 2-D texture atlas
//! 6. [`face`]    — public [`FontFace`] API tying everything together

// Glyph rasterization is inherently float↔int pixel math; these casts are
// bounded by atlas/bitmap dimensions and are correct by construction. Clippy's
// cast_possible_truncation/cast_sign_loss/cast_precision_loss lints fire on
// float<->int cast syntax unconditionally, with no clamp/try_from able to
// satisfy them for a float source (stable Rust has no checked/saturating
// float->int conversion API), so this one deliberate, reviewed crate-level
// allow covers every float<->int glyph-metric cast in this crate instead of
// scattering per-site allows or hand-rolling slower manual conversions with
// no behavior change.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

pub mod atlas;
pub mod outline;
pub mod parse;
pub mod raster;
pub mod tables;

pub mod face;
mod tests;

pub use atlas::{Atlas, GlyphEntry};
pub use face::{FontFace, GlyphMetrics, LoadError, RasterizedGlyph};
pub use raster::Bitmap;
pub use tables::GlyphId;
