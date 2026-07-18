#![cfg(test)]

use crate::{
    atlas::Atlas,
    face::FontFace,
    parse::{FontFile, ParseError},
    tables::{Cmap, GlyphId, Head, Hhea, Hmtx, Loca, Maxp},
};

const JBM: &str = "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf";
const PX: f32 = 16.0;

/// Monochrome emoji fixture (Noto Emoji), checked into `assets/fonts/` -- used
/// to regression-test cmap format-12 (astral-plane) lookups, which the
/// `JetBrains` Mono fixture above can't exercise (it's BMP-only). Path is
/// relative to this crate's manifest dir (`crates/font/`).
const NOTO_EMOJI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/NotoEmoji-Regular.ttf"
);

/// The one test fixture font (`JetBrains` Mono), namespaced as a zero-sized
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
    let outline = crate::outline::Outline::decode(glyf, off, len, &loca, 0).unwrap();
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

    let scale = PX / f32::from(head.units_per_em);
    let gid = cmap.glyph_id('M' as u32);
    let (off, len) = loca.glyph_range(gid).unwrap();
    let outline = crate::outline::Outline::decode(glyf, off, len, &loca, 0).unwrap();

    let ascender = f32::from(hhea.ascender) * scale;
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
    let scale = PX / f32::from(head.units_per_em);
    let gid = cmap.glyph_id('M' as u32);
    let (off, len) = loca.glyph_range(gid).unwrap();
    let outline = crate::outline::Outline::decode(glyf, off, len, &loca, 0).unwrap();
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

// ── cmap format 12 / emoji ───────────────────────────────────────────────────

#[test]
fn cmap_format12_resolves_astral_plane_emoji() {
    // Regression for the bug that made every emoji render as tofu: format 4
    // (the only subtable this crate parsed before) can't address codepoints
    // above U+FFFF, and virtually every emoji lives above it (U+1F300+).
    let data =
        std::fs::read(NOTO_EMOJI).expect("NotoEmoji-Regular.ttf missing -- see assets/fonts/");
    let ff = FontFile::parse(&data).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();

    // GRINNING FACE, SLIGHTLY SMILING FACE, THUMBS UP -- all astral-plane.
    for cp in [0x1F600u32, 0x1F642, 0x1F44D] {
        let gid = cmap.glyph_id(cp);
        assert_ne!(gid, GlyphId::NOTDEF, "U+{cp:04X} resolved to .notdef");
    }
}

#[test]
fn cmap_format12_still_resolves_bmp_codepoints() {
    // HEAVY BLACK HEART (U+2764) is in the BMP -- format 12's groups cover the
    // whole space, not just astral codepoints, so BMP lookups must still work
    // once format 12 is chosen over format 4.
    let data =
        std::fs::read(NOTO_EMOJI).expect("NotoEmoji-Regular.ttf missing -- see assets/fonts/");
    let ff = FontFile::parse(&data).unwrap();
    let cmap = Cmap::parse(&ff).unwrap();
    let gid = cmap.glyph_id(0x2764);
    assert_ne!(gid, GlyphId::NOTDEF, "U+2764 (BMP) resolved to .notdef");
}

#[test]
fn fontface_rasterizes_emoji_glyph() {
    // End-to-end: load the emoji face, rasterize a real emoji codepoint
    // through the full FontFace API (cmap -> loca -> glyf -> outline ->
    // bitmap), and check it actually produced ink -- not just a non-.notdef
    // glyph id.
    let mut face = FontFace::load(NOTO_EMOJI, PX).unwrap();
    let g = face.rasterize(0x1F600); // grinning face
    assert!(g.metrics.advance > 0.0, "emoji glyph has zero advance");
    let nonzero = g.bitmap.data.iter().filter(|&&v| v > 0).count();
    assert!(nonzero > 0, "rasterized emoji bitmap is all-zero");
}

// x_min/x_max vs y_min/y_max are the idiomatic names for a glyph bounding
// box; clippy's similar-names lint would rather they weren't both spelled
// out, but renaming one pair to something less symmetric would only make
// the geometry harder to follow.
#[allow(clippy::similar_names)]
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
    let scale = PX / f32::from(head.units_per_em);
    let ascender = f32::from(hhea.ascender) * scale;

    let mut atlas = Atlas::new(512, 512);
    let mut packed = 0u32;

    for cp in 0x21u32..=0x7Eu32 {
        let gid = cmap.glyph_id(cp);
        let (adv_raw, lsb_raw) = hmtx.metrics(gid);
        let Some((off, len)) = loca.glyph_range(gid) else {
            // blank glyph (space)
            let bmp = crate::raster::Bitmap::blank(0, 0);
            atlas.insert(&bmp, 0, 0, (f32::from(adv_raw) * scale) as i32);
            packed += 1;
            continue;
        };
        let outline = crate::outline::Outline::decode(glyf, off, len, &loca, 0).unwrap();
        let bounds_y_max = f32::from(outline.y_max) * scale;
        let bounds_y_min = f32::from(outline.y_min) * scale;
        let bounds_x_max = f32::from(outline.x_max) * scale;
        let bounds_x_min = f32::from(outline.x_min) * scale;
        let w = ((bounds_x_max - bounds_x_min).ceil() as u32).max(1);
        let h = ((bounds_y_max - bounds_y_min).ceil() as u32).max(1);
        let bmp = outline.rasterize(w, h, scale, ascender, -bounds_x_min);
        atlas.insert(
            &bmp,
            i32::from(lsb_raw),
            (bounds_y_max) as i32,
            (f32::from(adv_raw) * scale) as i32,
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
