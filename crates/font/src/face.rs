//! Public [`FontFace`] API — load a TTF/OTF file and render glyphs.

use std::collections::HashMap;

use crate::atlas::{Atlas, GlyphEntry};
use crate::parse::{FontFile, ParseError};
use crate::raster::Bitmap;
use crate::tables::{Cmap, Head, Hhea, Hmtx, Loca, Maxp};

// ── Public re-exports ─────────────────────────────────────────────────────────

/// Everything that can go wrong loading a font file from disk.
///
/// Kept distinct from [`ParseError`] (which is only about malformed font
/// bytes) so a missing/unreadable file is never misreported as "font data
/// too short" — that used to happen when [`FontFace::load`] collapsed every
/// `std::io::Error` into `ParseError::TooShort`.
#[derive(Debug)]
pub enum LoadError {
    /// The file couldn't be read (missing, permission denied, not a file, ...).
    Io(std::io::Error),
    /// The file was read fine but its bytes aren't a valid TTF/OTF.
    Parse(ParseError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "couldn't read font file: {e}"),
            Self::Parse(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
        }
    }
}

impl From<ParseError> for LoadError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

/// Per-glyph metrics in pixels (at the configured pixel size).
#[derive(Debug, Clone, Copy)]
pub struct GlyphMetrics {
    /// Horizontal advance in pixels.
    pub advance: f32,
    /// Left side bearing in pixels.
    pub bearing_x: f32,
    /// Distance from baseline to top of glyph bitmap, in pixels.
    pub bearing_y: f32,
    /// Glyph bitmap width in pixels.
    pub width: f32,
    /// Glyph bitmap height in pixels.
    pub height: f32,
}

/// A fully rasterized glyph (bitmap + metrics, not yet in an atlas).
#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    pub metrics: GlyphMetrics,
    pub bitmap: Bitmap,
}

// ── Font metrics (layout) ─────────────────────────────────────────────────────

/// Vertical metrics in pixels for the loaded size.
#[derive(Debug, Clone, Copy)]
pub struct LineMetrics {
    /// Distance from baseline to top of line box.
    pub ascender: f32,
    /// Distance from baseline to bottom of line box (negative = below).
    pub descender: f32,
    /// Recommended gap between lines.
    pub line_gap: f32,
    /// Full cell height = ascender - descender + `line_gap`.
    pub cell_height: f32,
}

// ── FontFace ──────────────────────────────────────────────────────────────────

/// Byte ranges of the tables we need, resolved once at load time so the
/// per-glyph hot path never re-walks the table directory.
#[derive(Clone, Copy)]
struct TableSpans {
    /// `(offset, len)` of the chosen cmap format-4 subtable.
    cmap_sub: (usize, usize),
    /// `(offset, len)` of the `loca` table.
    loca: (usize, usize),
    /// `(offset, len)` of the `hmtx` table.
    hmtx: (usize, usize),
    /// `(offset, len)` of the `glyf` table.
    glyf: (usize, usize),
    loca_format: i16,
    number_of_hmetrics: u16,
}

/// A loaded font face at a specific pixel size.
///
/// Owns the raw font bytes and a glyph cache. Thread-unsafe by design —
/// wrap in a `Mutex` if you need to share across threads (we don't in this
/// single-renderer-thread design).
pub struct FontFace {
    data: Vec<u8>,
    scale: f32, // font-units → pixels
    units_per_em: u16,
    spans: TableSpans,
    pub line: LineMetrics,
    /// Advance width of the reference glyph ('M'), used as cell width.
    pub cell_width: f32,
    cache: HashMap<u32, RasterizedGlyph>,
}

impl FontFace {
    /// Load a TTF/OTF file from disk and configure it at `px_size` pixels tall.
    ///
    /// # Errors
    /// Returns [`LoadError::Io`] if the file can't be read (missing, permission
    /// denied, ...) or [`LoadError::Parse`] if its bytes aren't a valid TTF/OTF.
    pub fn load(path: impl AsRef<std::path::Path>, px_size: f32) -> Result<Self, LoadError> {
        let data = std::fs::read(path).map_err(LoadError::Io)?;
        Ok(Self::from_bytes(data, px_size)?)
    }

    /// Parse from already-loaded bytes.
    ///
    /// # Errors
    /// Returns [`ParseError`] on malformed font data.
    pub fn from_bytes(data: Vec<u8>, px_size: f32) -> Result<Self, ParseError> {
        let ff = FontFile::parse(&data)?;
        let head = Head::parse(&ff)?;
        let hhea = Hhea::parse(&ff)?;
        Maxp::parse(&ff)?; // validate maxp / num_glyphs is present

        let scale = px_size / f32::from(head.units_per_em);

        let ascender = f32::from(hhea.ascender) * scale;
        let descender = f32::from(hhea.descender) * scale;
        let line_gap = f32::from(hhea.line_gap) * scale;
        let cell_height = ascender - descender + line_gap;

        // Resolve table byte-spans once. `require` validates each table exists.
        let cmap_whole = ff.require(crate::parse::TAG_CMAP, "cmap")?;
        let cmap_off = Cmap::find_subtable(cmap_whole)?;
        let cmap_rec = ff.tables[crate::parse::TAG_CMAP].ok_or(ParseError::MissingTable("cmap"))?;
        let loca_rec = ff.tables[crate::parse::TAG_LOCA].ok_or(ParseError::MissingTable("loca"))?;
        let hmtx_rec = ff.tables[crate::parse::TAG_HMTX].ok_or(ParseError::MissingTable("hmtx"))?;
        let glyf_rec = ff.tables[crate::parse::TAG_GLYF].ok_or(ParseError::MissingTable("glyf"))?;

        let spans = TableSpans {
            cmap_sub: (cmap_rec.offset + cmap_off, cmap_rec.length - cmap_off),
            loca: (loca_rec.offset, loca_rec.length),
            hmtx: (hmtx_rec.offset, hmtx_rec.length),
            glyf: (glyf_rec.offset, glyf_rec.length),
            loca_format: head.index_to_loc_format,
            number_of_hmetrics: hhea.number_of_hmetrics,
        };

        // Cell width = advance of glyph 'M' (or fallback to glyph 0).
        let cmap =
            Cmap::from_subtable(&data[spans.cmap_sub.0..spans.cmap_sub.0 + spans.cmap_sub.1]);
        let hmtx = Hmtx::from_bytes(
            &data[spans.hmtx.0..spans.hmtx.0 + spans.hmtx.1],
            spans.number_of_hmetrics,
        );
        let m_gid = cmap.glyph_id('M' as u32);
        let (adv, _) = hmtx.metrics(m_gid);
        let cell_width = f32::from(adv) * scale;

        Ok(Self {
            data,
            scale,
            units_per_em: head.units_per_em,
            spans,
            line: LineMetrics {
                ascender,
                descender,
                line_gap,
                cell_height,
            },
            cell_width,
            cache: HashMap::new(),
        })
    }

    // Table accessors over the cached spans — pure slice indexing, no re-parse.
    fn cmap(&self) -> Cmap<'_> {
        let (o, l) = self.spans.cmap_sub;
        Cmap::from_subtable(&self.data[o..o + l])
    }
    fn hmtx(&self) -> Hmtx<'_> {
        let (o, l) = self.spans.hmtx;
        Hmtx::from_bytes(&self.data[o..o + l], self.spans.number_of_hmetrics)
    }
    fn loca(&self) -> Loca<'_> {
        let (o, l) = self.spans.loca;
        Loca::from_bytes(&self.data[o..o + l], self.spans.loca_format)
    }
    fn glyf(&self) -> &[u8] {
        let (o, l) = self.spans.glyf;
        &self.data[o..o + l]
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
        let cmap = self.cmap();
        let hmtx = self.hmtx();
        let loca = self.loca();

        let gid = cmap.glyph_id(codepoint);
        let (adv_raw, lsb_raw) = hmtx.metrics(gid);

        let scale = self.scale;

        let advance = f32::from(adv_raw) * scale;
        let bearing_x = f32::from(lsb_raw) * scale;

        // Get the glyph outline.
        let outline =
            loca.glyph_range(gid)
                .map_or_else(crate::outline::Outline::default, |(off, len)| {
                    crate::outline::Outline::decode(self.glyf(), off, len, &loca, 0)
                        .unwrap_or_default()
                });

        // Compute pixel-space bounding box from font-unit bbox.
        let top = f32::from(outline.y_max) * scale;
        let bottom = f32::from(outline.y_min) * scale;
        let right = f32::from(outline.x_max) * scale;
        let left = f32::from(outline.x_min) * scale;

        let bmp_w = ((right - left).ceil() as u32).max(1);
        let bmp_h = ((top - bottom).ceil() as u32).max(1);

        let bearing_y = top; // top of glyph above baseline

        // Rasterize: baseline sits at `top` inside the bitmap,
        // glyph x starts at `left`.
        let bitmap = if outline.verbs.is_empty() {
            Bitmap::blank(0, 0)
        } else {
            outline.rasterize(bmp_w, bmp_h, scale, top, -left)
        };

        let metrics = GlyphMetrics {
            advance,
            bearing_x,
            bearing_y,
            width: bitmap.width as f32,
            height: bitmap.height as f32,
        };

        RasterizedGlyph { metrics, bitmap }
    }

    /// Combine a font's (fractional) ascender with a glyph's (fractional)
    /// baseline-to-top bearing into the single pixel offset from the cell's
    /// top row down to the glyph bitmap's top row, rounding **once**.
    ///
    /// This must not be done as `ascender.round() - bearing_y.round()`: each
    /// glyph's `bearing_y` has its own fractional part, so rounding it
    /// separately from the (shared, but also fractional) ascender makes
    /// every glyph's baseline independently jitter by up to a pixel relative
    /// to its neighbors — the general "baseline looks a little off" bug.
    /// Rounding the combined offset once keeps every glyph's baseline
    /// pinned to the exact same cell-relative row.
    #[must_use]
    pub fn cell_bearing_y(ascender: f32, bearing_y: f32) -> i32 {
        (ascender - bearing_y).round() as i32
    }

    /// Build a [`Atlas`] pre-populated with the printable ASCII glyphs (0x20–0x7E)
    /// plus a handful of box-drawing characters. This covers 99 % of terminal output.
    ///
    /// Returns `(atlas, glyph_map)` where `glyph_map` maps codepoint → [`GlyphEntry`].
    pub fn build_ascii_atlas(&mut self, atlas_size: u32) -> (Atlas, HashMap<u32, GlyphEntry>) {
        let mut atlas = Atlas::new(atlas_size, atlas_size);
        let mut map = HashMap::new();

        // Printable ASCII + common box-drawing block.
        let codepoints: Vec<u32> = (0x20u32..=0x7E)
            .chain(0x2500u32..=0x257F) // box-drawing
            .chain(0x2580u32..=0x259F) // block elements
            .collect();

        for cp in codepoints {
            let g = self.do_rasterize(cp);
            let entry = atlas.insert(
                &g.bitmap,
                g.metrics.bearing_x.round() as i32,
                Self::cell_bearing_y(self.line.ascender, g.metrics.bearing_y),
                g.metrics.advance.round() as i32,
            );
            if let Some(e) = entry {
                map.insert(cp, e);
            }
        }

        (atlas, map)
    }

    /// Scale factor: font units → pixels.
    #[must_use]
    pub const fn scale(&self) -> f32 {
        self.scale
    }

    /// Font units per em.
    #[must_use]
    pub const fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
}
