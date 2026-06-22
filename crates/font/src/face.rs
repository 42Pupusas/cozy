//! Public [`FontFace`] API — load a TTF/OTF file and render glyphs.

use std::collections::HashMap;

use crate::atlas::{Atlas, GlyphEntry};
use crate::outline::decode_glyph;
use crate::parse::{FontFile, ParseError, TAG_GLYF};
use crate::raster::{Bitmap, rasterize};
use crate::tables::{Cmap, Head, Hhea, Hmtx, Loca, Maxp};

// ── Public re-exports ─────────────────────────────────────────────────────────

/// Per-glyph metrics in pixels (at the configured pixel size).
#[derive(Debug, Clone, Copy)]
pub struct GlyphMetrics {
    /// Horizontal advance in pixels.
    pub advance:   f32,
    /// Left side bearing in pixels.
    pub bearing_x: f32,
    /// Distance from baseline to top of glyph bitmap, in pixels.
    pub bearing_y: f32,
    /// Glyph bitmap width in pixels.
    pub width:     f32,
    /// Glyph bitmap height in pixels.
    pub height:    f32,
}

/// A fully rasterized glyph (bitmap + metrics, not yet in an atlas).
#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    pub metrics: GlyphMetrics,
    pub bitmap:  Bitmap,
}

// ── Font metrics (layout) ─────────────────────────────────────────────────────

/// Vertical metrics in pixels for the loaded size.
#[derive(Debug, Clone, Copy)]
pub struct LineMetrics {
    /// Distance from baseline to top of line box.
    pub ascender:  f32,
    /// Distance from baseline to bottom of line box (negative = below).
    pub descender: f32,
    /// Recommended gap between lines.
    pub line_gap:  f32,
    /// Full cell height = ascender - descender + line_gap.
    pub cell_height: f32,
}

// ── FontFace ──────────────────────────────────────────────────────────────────

/// A loaded font face at a specific pixel size.
///
/// Owns the raw font bytes and a glyph cache. Thread-unsafe by design —
/// wrap in a `Mutex` if you need to share across threads (we don't in this
/// single-renderer-thread design).
pub struct FontFace {
    data:          Vec<u8>,
    scale:         f32, // font-units → pixels
    units_per_em:  u16,
    pub line:      LineMetrics,
    /// Advance width of the reference glyph ('M'), used as cell width.
    pub cell_width: f32,
    cache:         HashMap<u32, RasterizedGlyph>,
}

impl FontFace {
    /// Load a TTF/OTF file from disk and configure it at `px_size` pixels tall.
    ///
    /// # Errors
    /// Returns [`ParseError`] on malformed font data.
    pub fn load(path: impl AsRef<std::path::Path>, px_size: f32) -> Result<Self, ParseError> {
        let data = std::fs::read(path).map_err(|_| ParseError::TooShort)?;
        Self::from_bytes(data, px_size)
    }

    /// Parse from already-loaded bytes.
    ///
    /// # Errors
    /// Returns [`ParseError`] on malformed font data.
    pub fn from_bytes(data: Vec<u8>, px_size: f32) -> Result<Self, ParseError> {
        let ff    = FontFile::parse(&data)?;
        let head  = Head::parse(&ff)?;
        let hhea  = Hhea::parse(&ff)?;
        let maxp  = Maxp::parse(&ff)?;
        let hmtx  = Hmtx::parse(&ff, hhea.number_of_hmetrics)?;
        let _cmap = Cmap::parse(&ff)?; // validate cmap exists
        let _loca = Loca::parse(&ff, head.index_to_loc_format)?; // validate loca
        let _ = maxp; // num_glyphs validated implicitly

        let scale = px_size / head.units_per_em as f32;

        let ascender    = hhea.ascender  as f32 * scale;
        let descender   = hhea.descender as f32 * scale;
        let line_gap    = hhea.line_gap  as f32 * scale;
        let cell_height = ascender - descender + line_gap;

        // Cell width = advance of glyph 'M' (or fallback to glyph 0).
        let cmap       = Cmap::parse(&ff)?;
        let m_gid      = cmap.glyph_id('M' as u32);
        let (adv, _)   = hmtx.metrics(m_gid);
        let cell_width = adv as f32 * scale;

        Ok(Self {
            data,
            scale,
            units_per_em: head.units_per_em,
            line: LineMetrics { ascender, descender, line_gap, cell_height },
            cell_width,
            cache: HashMap::new(),
        })
    }

    /// Rasterize a Unicode codepoint and return the result (cached).
    pub fn rasterize(&mut self, codepoint: u32) -> &RasterizedGlyph {
        if !self.cache.contains_key(&codepoint) {
            let g = self.do_rasterize(codepoint);
            self.cache.insert(codepoint, g);
        }
        self.cache.get(&codepoint).expect("just inserted")
    }

    fn do_rasterize(&self, codepoint: u32) -> RasterizedGlyph {
        let ff   = FontFile::parse(&self.data).expect("already validated");
        let head = Head::parse(&ff).expect("already validated");
        let hhea = Hhea::parse(&ff).expect("already validated");
        let hmtx = Hmtx::parse(&ff, hhea.number_of_hmetrics).expect("already validated");
        let cmap = Cmap::parse(&ff).expect("already validated");
        let loca = Loca::parse(&ff, head.index_to_loc_format).expect("already validated");

        let gid = cmap.glyph_id(codepoint);
        let (adv_raw, lsb_raw) = hmtx.metrics(gid);

        let scale  = self.scale;
        let ascender = hhea.ascender as f32 * scale;

        let advance   = adv_raw as f32 * scale;
        let bearing_x = lsb_raw as f32 * scale;

        // Get the glyph outline.
        let outline = if let Some((off, len)) = loca.glyph_range(gid) {
            if let Some(glyf_data) = ff.table(TAG_GLYF) {
                decode_glyph(glyf_data, off, len, 0)
                    .unwrap_or_default()
            } else {
                crate::outline::Outline::default()
            }
        } else {
            crate::outline::Outline::default() // space / empty glyph
        };

        // Compute pixel-space bounding box from font-unit bbox.
        let px_y_max = outline.y_max as f32 * scale;
        let px_y_min = outline.y_min as f32 * scale;
        let px_x_max = outline.x_max as f32 * scale;
        let px_x_min = outline.x_min as f32 * scale;

        let bmp_w = ((px_x_max - px_x_min).ceil() as u32).max(1);
        let bmp_h = ((px_y_max - px_y_min).ceil() as u32).max(1);

        let bearing_y = px_y_max; // top of glyph above baseline

        // Rasterize: baseline sits at ascender inside the bitmap,
        // glyph x starts at bearing_x.
        let bitmap = if outline.verbs.is_empty() {
            Bitmap::blank(0, 0)
        } else {
            let baseline = px_y_max; // flip: 0 is top of bitmap
            rasterize(&outline, bmp_w, bmp_h, scale, baseline, -px_x_min)
        };

        let metrics = GlyphMetrics {
            advance,
            bearing_x,
            bearing_y,
            width:  bitmap.width  as f32,
            height: bitmap.height as f32,
        };

        RasterizedGlyph { metrics, bitmap }
    }

    /// Build a [`Atlas`] pre-populated with the printable ASCII glyphs (0x20–0x7E)
    /// plus a handful of box-drawing characters. This covers 99 % of terminal output.
    ///
    /// Returns `(atlas, glyph_map)` where `glyph_map` maps codepoint → [`GlyphEntry`].
    pub fn build_ascii_atlas(&mut self, atlas_size: u32) -> (Atlas, HashMap<u32, GlyphEntry>) {
        let mut atlas = Atlas::new(atlas_size, atlas_size);
        let mut map   = HashMap::new();

        // Printable ASCII + common box-drawing block.
        let codepoints: Vec<u32> = (0x20u32..=0x7E)
            .chain(0x2500u32..=0x257F) // box-drawing
            .chain(0x2580u32..=0x259F) // block elements
            .collect();

        for cp in codepoints {
            let g = self.do_rasterize(cp);
            let entry = atlas.insert(
                &g.bitmap,
                g.metrics.bearing_x as i32,
                g.metrics.bearing_y as i32,
                g.metrics.advance   as i32,
            );
            if let Some(e) = entry {
                map.insert(cp, e);
            }
        }

        (atlas, map)
    }

    /// Scale factor: font units → pixels.
    #[must_use]
    pub const fn scale(&self) -> f32 { self.scale }

    /// Font units per em.
    #[must_use]
    pub const fn units_per_em(&self) -> u16 { self.units_per_em }
}
