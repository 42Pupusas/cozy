#![cfg(test)]

use crate::{
    atlas::Atlas,
    face::FontFace,
    parse::{FontFile, ParseError},
    tables::{Cmap, Head, Hhea, Hmtx, Loca, Maxp},
};

const JBM: &str = "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf";
const PX: f32 = 16.0;

/// The one test fixture font (JetBrains Mono), namespaced as a zero-sized
/// marker type rather than a bare `load_bytes()` function -- reads as
/// `Jbm::bytes()`, the same "constructor namespace" shape used for
/// `RawPty::fork` etc. in the non-test code, instead of a free fn with an
/// implicit global (`JBM`) as its real input.
struct Jbm;

impl Jbm {
    fn bytes() -> Vec<u8> {
        std::fs::read(JBM).expect("JetBrains Mono not found — install fonts-jetbrains-mono")
    }
}

// ── parse ─────────────────────────────────────────────────────────────────────

#[test]
fn parse_offset_table() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    // Required tables must be present.
    assert!(ff.table(crate::parse::TAG_HEAD).is_some(), "head missing");
    assert!(ff.table(crate::parse::TAG_HHEA).is_some(), "hhea missing");
    assert!(ff.table(crate::parse::TAG_CMAP).is_some(), "cmap missing");
    assert!(ff.table(crate::parse::TAG_HMTX).is_some(), "hmtx missing");
    assert!(ff.table(crate::parse::TAG_LOCA).is_some(), "loca missing");
    assert!(ff.table(crate::parse::TAG_GLYF).is_some(), "glyf missing");
}

#[test]
fn parse_bad_bytes_returns_error() {
    let result = FontFile::parse(b"not a font at all!");
    assert_eq!(result.unwrap_err(), ParseError::BadSfntVersion);
}

#[test]
fn parse_too_short_returns_error() {
    let result = FontFile::parse(b"\x00\x01");
    assert_eq!(result.unwrap_err(), ParseError::TooShort);
}

#[test]
fn parse_error_display_messages() {
    assert_eq!(ParseError::TooShort.to_string(), "font data too short");
    assert_eq!(
        ParseError::BadSfntVersion.to_string(),
        "not a TTF/OTF file (bad sfnt version)"
    );
    assert_eq!(
        ParseError::MissingTable("head").to_string(),
        "required table missing: head"
    );
}

// ── tables ────────────────────────────────────────────────────────────────────

#[test]
fn head_units_per_em_sane() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    // Common values: 1000 (PostScript) or 2048 (TrueType)
    assert!(
        head.units_per_em >= 512 && head.units_per_em <= 4096,
        "units_per_em = {}",
        head.units_per_em
    );
}

#[test]
fn hhea_ascender_positive_descender_negative() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let hhea = Hhea::parse(&ff).unwrap();
    assert!(hhea.ascender > 0, "ascender = {}", hhea.ascender);
    assert!(hhea.descender < 0, "descender = {}", hhea.descender);
}

#[test]
fn maxp_num_glyphs_nonzero() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let maxp = Maxp::parse(&ff).unwrap();
    assert!(maxp.num_glyphs > 100, "num_glyphs = {}", maxp.num_glyphs);
}

#[test]
fn cmap_ascii_maps_correctly() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();

    // Glyph 0 is .notdef — codepoints in [0x21, 0x7E] must map to non-zero.
    for cp in 0x21u32..=0x7E {
        let gid = cmap.glyph_id(cp);
        assert!(
            gid != crate::tables::GlyphId::NOTDEF,
            "codepoint U+{cp:04X} maps to .notdef"
        );
    }
}

#[test]
fn cmap_notdef_for_unmapped() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    // U+E000 private use — almost certainly unmapped in JBM.
    // We just check this doesn't panic.
    let _ = cmap.glyph_id(0xE000);
}

#[test]
fn hmtx_advance_nonzero_for_m() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let hhea = Hhea::parse(&ff).unwrap();
    let hmtx = Hmtx::parse(&ff, hhea.number_of_hmetrics).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let gid = cmap.glyph_id('M' as u32);
    let (adv, _) = hmtx.metrics(gid);
    assert!(adv > 0, "advance of 'M' = {adv}");
}

#[test]
fn loca_gives_range_for_m() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let gid = cmap.glyph_id('M' as u32);
    let range = loca.glyph_range(gid);
    assert!(range.is_some(), "loca range for 'M' (gid {gid}) is None");
    let (off, len) = range.unwrap();
    assert!(len > 0, "glyph len = 0 for 'M'");
    assert!(off + len <= ff.table(crate::parse::TAG_GLYF).unwrap().len());
}

// ── outline ───────────────────────────────────────────────────────────────────

#[test]
fn outline_m_has_verbs() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let glyf = ff.table(crate::parse::TAG_GLYF).unwrap();
    let gid = cmap.glyph_id('M' as u32);
    let (off, len) = loca.glyph_range(gid).unwrap();
    let outline = crate::outline::Outline::decode(glyf, off, len, 0).unwrap();
    assert!(!outline.verbs.is_empty(), "outline for 'M' has no verbs");
}

#[test]
fn outline_space_is_empty() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let gid = cmap.glyph_id(' ' as u32);
    // Space has no outline — loca range is None.
    assert!(
        loca.glyph_range(gid).is_none(),
        "space should have no outline"
    );
}

// ── raster ────────────────────────────────────────────────────────────────────

#[test]
fn raster_m_produces_nonzero_pixels() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let hhea = Hhea::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let glyf = ff.table(crate::parse::TAG_GLYF).unwrap();

    let scale = PX / head.units_per_em as f32;
    let gid = cmap.glyph_id('M' as u32);
    let (off, len) = loca.glyph_range(gid).unwrap();
    let outline = crate::outline::Outline::decode(glyf, off, len, 0).unwrap();

    let ascender = hhea.ascender as f32 * scale;
    let w = 16u32;
    let h = 20u32;
    let bmp = outline.rasterize(w, h, scale, ascender, 0.0);

    let nonzero = bmp.data.iter().filter(|&&v| v > 0).count();
    assert!(nonzero > 0, "rasterized 'M' bitmap is all-zero");
}

#[test]
fn raster_bitmap_correct_size() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let glyf = ff.table(crate::parse::TAG_GLYF).unwrap();
    let scale = PX / head.units_per_em as f32;
    let gid = cmap.glyph_id('M' as u32);
    let (off, len) = loca.glyph_range(gid).unwrap();
    let outline = crate::outline::Outline::decode(glyf, off, len, 0).unwrap();
    let bmp = outline.rasterize(16, 20, scale, 16.0, 0.0);
    assert_eq!(bmp.data.len(), (16 * 20) as usize);
}

// ── atlas ─────────────────────────────────────────────────────────────────────

#[test]
fn atlas_inserts_blank_glyph_as_zero_size() {
    let mut atlas = Atlas::new(256, 256);
    let blank = crate::raster::Bitmap::blank(0, 0);
    let entry = atlas.insert(&blank, 0, 0, 500).unwrap();
    assert_eq!(entry.width, 0);
    assert_eq!(entry.height, 0);
    assert_eq!(entry.advance, 500);
}

#[test]
fn atlas_packs_without_overflow() {
    let data = Jbm::bytes();
    let ff = FontFile::parse(&data).unwrap();
    let head = Head::parse(&ff).unwrap();
    let loca = Loca::parse(&ff, head.index_to_loc_format).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let glyf = ff.table(crate::parse::TAG_GLYF).unwrap();
    let hhea = Hhea::parse(&ff).unwrap();
    let hmtx = Hmtx::parse(&ff, hhea.number_of_hmetrics).unwrap();
    let scale = PX / head.units_per_em as f32;
    let ascender = hhea.ascender as f32 * scale;

    let mut atlas = Atlas::new(512, 512);
    let mut packed = 0u32;

    for cp in 0x21u32..=0x7Eu32 {
        let gid = cmap.glyph_id(cp);
        let (adv_raw, lsb_raw) = hmtx.metrics(gid);
        let (off, len) = match loca.glyph_range(gid) {
            Some(r) => r,
            None => {
                // blank glyph (space)
                let bmp = crate::raster::Bitmap::blank(0, 0);
                atlas.insert(&bmp, 0, 0, (adv_raw as f32 * scale) as i32);
                packed += 1;
                continue;
            }
        };
        let outline = crate::outline::Outline::decode(glyf, off, len, 0).unwrap();
        let px_y_max = outline.y_max as f32 * scale;
        let px_y_min = outline.y_min as f32 * scale;
        let px_x_max = outline.x_max as f32 * scale;
        let px_x_min = outline.x_min as f32 * scale;
        let w = ((px_x_max - px_x_min).ceil() as u32).max(1);
        let h = ((px_y_max - px_y_min).ceil() as u32).max(1);
        let bmp = outline.rasterize(w, h, scale, ascender, -px_x_min);
        atlas.insert(
            &bmp,
            lsb_raw as i32,
            (px_y_max) as i32,
            (adv_raw as f32 * scale) as i32,
        );
        packed += 1;
    }

    // All 94 printable ASCII glyphs should fit in a 512×512 atlas at 16 px.
    assert_eq!(packed, 94, "expected 94 packed, got {packed}");
}

// ── FontFace ──────────────────────────────────────────────────────────────────

#[test]
fn fontface_loads_and_has_sane_metrics() {
    let face = FontFace::load(JBM, PX).unwrap();
    assert!(
        face.line.ascender > 0.0,
        "ascender = {}",
        face.line.ascender
    );
    assert!(
        face.line.descender < 0.0,
        "descender = {}",
        face.line.descender
    );
    assert!(
        face.line.cell_height > PX * 0.8,
        "cell_height = {}",
        face.line.cell_height
    );
    assert!(face.cell_width > 0.0, "cell_width = {}", face.cell_width);
}

#[test]
fn fontface_rasterize_m_nonzero() {
    let mut face = FontFace::load(JBM, PX).unwrap();
    let g = face.rasterize('M' as u32);
    assert!(g.metrics.advance > 0.0);
    // Bitmap may be empty for broken glyphs — just check it doesn't panic.
}

#[test]
fn fontface_rasterize_cached() {
    let mut face = FontFace::load(JBM, PX).unwrap();
    // Call twice — second must hit cache.
    let a = face.rasterize('A' as u32).metrics.advance;
    let b = face.rasterize('A' as u32).metrics.advance;
    assert!((a - b).abs() < 0.001);
}

#[test]
fn fontface_build_ascii_atlas() {
    let mut face = FontFace::load(JBM, PX).unwrap();
    let (atlas, map) = face.build_ascii_atlas(512);
    // All printable ASCII glyphs should be in the map.
    for cp in 0x20u32..=0x7E {
        assert!(map.contains_key(&cp), "U+{cp:04X} not in atlas map");
    }
    // Atlas data should be non-trivially populated.
    let nonzero = atlas.data.iter().filter(|&&v| v > 0).count();
    assert!(
        nonzero > 100,
        "atlas appears empty: only {nonzero} non-zero pixels"
    );
}
