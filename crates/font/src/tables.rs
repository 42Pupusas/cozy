//! Decoded TTF table structs: head, hhea, maxp, cmap (fmt 4), loca, hmtx.

use crate::parse::FontFile;
use crate::parse::{
    ByteSliceExt, ParseError, TAG_CMAP, TAG_HEAD, TAG_HHEA, TAG_HMTX, TAG_LOCA, TAG_MAXP,
};

/// A glyph index into the font's `glyf`/`hmtx`/`loca` tables (`0` = `.notdef`).
///
/// Newtyped so it can't be silently confused with any other `u16` floating
/// around this module (`number_of_hmetrics`, table lengths, Unicode BMP
/// scalars truncated to 16 bits, …) -- [`Cmap::glyph_id`] is the only way to
/// produce one, and [`Loca::glyph_range`] / [`Hmtx::metrics`] /
/// [`crate::outline::Outline::decode`] are the only things that consume one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlyphId(pub u16);

impl GlyphId {
    /// `.notdef` -- the glyph shown for codepoints missing from the font.
    pub const NOTDEF: Self = Self(0);
}

impl std::fmt::Display for GlyphId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── head ─────────────────────────────────────────────────────────────────────

pub struct Head {
    pub units_per_em: u16,
    pub index_to_loc_format: i16, // 0 = short, 1 = long
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

impl Head {
    pub fn parse(ff: &FontFile<'_>) -> Result<Self, ParseError> {
        let d = ff.require(TAG_HEAD, "head")?;
        Ok(Self {
            units_per_em: d.u16_at(18).ok_or(ParseError::TooShort)?,
            index_to_loc_format: d.i16_at(50).ok_or(ParseError::TooShort)?,
            x_min: d.i16_at(36).ok_or(ParseError::TooShort)?,
            y_min: d.i16_at(38).ok_or(ParseError::TooShort)?,
            x_max: d.i16_at(40).ok_or(ParseError::TooShort)?,
            y_max: d.i16_at(42).ok_or(ParseError::TooShort)?,
        })
    }
}

// ── hhea ─────────────────────────────────────────────────────────────────────

pub struct Hhea {
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub number_of_hmetrics: u16,
}

impl Hhea {
    pub fn parse(ff: &FontFile<'_>) -> Result<Self, ParseError> {
        let d = ff.require(TAG_HHEA, "hhea")?;
        Ok(Self {
            ascender: d.i16_at(4).ok_or(ParseError::TooShort)?,
            descender: d.i16_at(6).ok_or(ParseError::TooShort)?,
            line_gap: d.i16_at(8).ok_or(ParseError::TooShort)?,
            number_of_hmetrics: d.u16_at(34).ok_or(ParseError::TooShort)?,
        })
    }
}

// ── maxp ─────────────────────────────────────────────────────────────────────

pub struct Maxp {
    pub num_glyphs: u16,
}

impl Maxp {
    pub fn parse(ff: &FontFile<'_>) -> Result<Self, ParseError> {
        let d = ff.require(TAG_MAXP, "maxp")?;
        Ok(Self {
            num_glyphs: d.u16_at(4).ok_or(ParseError::TooShort)?,
        })
    }
}

// ── cmap (format 4 — BMP-only; format 12 — full 32-bit Unicode) ─────────────
//
// Format 4's 16-bit char codes can only address the Basic Multilingual
// Plane (U+0000..=U+FFFF). Emoji live almost entirely in the astral planes
// (U+1F300 "GRAPE" and friends start at U+1F000+), so a font that wants to
// map them needs a format-12 "segmented coverage" subtable, whose group
// entries carry full 32-bit `startCharCode`/`endCharCode`. Without this, any
// codepoint above U+FFFF silently resolves to `.notdef` even when the font
// (e.g. Noto Emoji) has the glyph -- exactly the bug that made emoji render
// as tofu/blank before this was added.

/// Map a Unicode codepoint → glyph id. Wraps whichever subtable format
/// [`Self::find_subtable`] selected (4 or 12); [`Self::glyph_id`] dispatches
/// on the format recorded at construction time.
pub struct Cmap<'a> {
    data: &'a [u8], // the chosen subtable slice (format 4 or 12)
    format: u16,
}

impl<'a> Cmap<'a> {
    pub fn parse(ff: &FontFile<'a>) -> Result<Self, ParseError> {
        let d = ff.require(TAG_CMAP, "cmap")?;
        let sub_off = Self::find_subtable(d)?;
        Ok(Self::from_subtable(&d[sub_off..]))
    }

    /// Wrap an already-located subtable slice (no directory walk). The
    /// subtable's own `format` field (first `u16`) is read to decide how
    /// [`Self::glyph_id`] interprets the rest of the bytes -- unlike the
    /// pre-format-12 version, this can no longer be a `const fn` (format is
    /// discovered at runtime, not baked in by the caller).
    #[must_use]
    pub fn from_subtable(data: &'a [u8]) -> Self {
        let format = data.u16_at(0).unwrap_or(0);
        Self { data, format }
    }

    /// Locate the best cmap subtable within a whole `cmap` table, preferring
    /// (in order) a format-12 full-Unicode subtable — needed for astral-plane
    /// codepoints like emoji — then the universal format-4 BMP subtable.
    /// Returns the offset of the subtable relative to the `cmap` table start.
    ///
    /// # Errors
    /// [`ParseError`] if no usable format-4/format-12 subtable is present.
    pub fn find_subtable(d: &[u8]) -> Result<usize, ParseError> {
        let num_tables = d.u16_at(2).ok_or(ParseError::TooShort)? as usize;

        let mut best: Option<(u8, usize)> = None;
        for i in 0..num_tables {
            let base = 4 + i * 8;
            let platform = d.u16_at(base).ok_or(ParseError::TooShort)?;
            let encoding = d.u16_at(base + 2).ok_or(ParseError::TooShort)?;
            let offset = d.u32_at(base + 4).ok_or(ParseError::TooShort)? as usize;
            let format = d.u16_at(offset).ok_or(ParseError::TooShort)?;
            if let Some(r) = Self::rank_subtable(format, platform, encoding)
                && best.is_none_or(|(br, _)| r > br)
            {
                best = Some((r, offset));
            }
        }

        best.map(|(_, off)| off)
            .ok_or(ParseError::MissingTable("cmap format 4/12"))
    }

    /// Rank a candidate cmap subtable; higher wins, `None` if unusable.
    /// Format 12 outranks format 4 (it's a superset — BMP codepoints resolve
    /// identically, astral ones only resolve via format 12), and within a
    /// format the Windows-platform encoding is preferred as most
    /// interoperable.
    const fn rank_subtable(format: u16, platform: u16, encoding: u16) -> Option<u8> {
        match (format, platform, encoding) {
            (12, 3, 10) => Some(4),
            (12, 0, 4 | 6) => Some(3),
            (4, 3, 1) => Some(2),
            (4, 0, _) => Some(1),
            _ => None,
        }
    }

    /// Map a Unicode scalar to a glyph index (`GlyphId::NOTDEF` if unmapped).
    #[must_use]
    pub fn glyph_id(&self, codepoint: u32) -> GlyphId {
        match self.format {
            12 => self.glyph_id_fmt12(codepoint),
            _ => self.glyph_id_fmt4(codepoint),
        }
    }

    /// Format 4 (segment mapping to delta values): the universal BMP
    /// subtable. `codepoint` is truncated to 16 bits — any caller routing
    /// an astral-plane codepoint here instead of a format-12 subtable will
    /// simply miss (by construction: format 4 has no 17th bit to match).
    fn glyph_id_fmt4(&self, codepoint: u32) -> GlyphId {
        let d = self.data;
        // format=4, length, language already consumed; seg_count_x2 at offset 6
        let seg_count = match d.u16_at(6) {
            Some(v) => (v / 2) as usize,
            None => return GlyphId::NOTDEF,
        };

        let end_codes_off = 14;
        let start_codes_off = 14 + seg_count * 2 + 2; // skip reservedPad
        let id_delta_off = start_codes_off + seg_count * 2;
        let id_range_off = id_delta_off + seg_count * 2;
        let glyph_ids_off = id_range_off + seg_count * 2;

        let cp = codepoint as u16;

        // Binary search over end_codes.
        let mut lo = 0usize;
        let mut hi = seg_count;
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            let end = d.u16_at(end_codes_off + mid * 2).unwrap_or(0);
            if cp > end {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= seg_count {
            return GlyphId::NOTDEF;
        }

        let start = d.u16_at(start_codes_off + lo * 2).unwrap_or(0xFFFF);
        if cp < start {
            return GlyphId::NOTDEF;
        }

        let id_range_offset = d.u16_at(id_range_off + lo * 2).unwrap_or(0) as usize;
        if id_range_offset == 0 {
            let delta = i32::from(d.i16_at(id_delta_off + lo * 2).unwrap_or(0));
            let raw = (i32::from(cp) + delta) & 0xFFFF;
            return GlyphId(raw as u16);
        }

        // id_range_offset is relative to its own position in the table.
        let slot_pos = id_range_off + lo * 2;
        let glyph_off = slot_pos + id_range_offset + (cp - start) as usize * 2;
        let glyph_off = glyph_off.saturating_sub(0); // relative to subtable start
        if glyph_off >= glyph_ids_off {
            let idx = glyph_off - glyph_ids_off;
            let raw = d.u16_at(glyph_ids_off + idx).unwrap_or(0);
            if raw == 0 {
                return GlyphId::NOTDEF;
            }
            let delta = i32::from(d.i16_at(id_delta_off + lo * 2).unwrap_or(0));
            return GlyphId(((i32::from(raw) + delta) & 0xFFFF) as u16);
        }
        GlyphId::NOTDEF
    }

    /// Format 12 (segmented coverage): groups of
    /// `(startCharCode, endCharCode, startGlyphID)` as full 32-bit values —
    /// the only cmap format that can address codepoints above U+FFFF, which
    /// is where the overwhelming majority of emoji live (U+1F300+).
    ///
    /// Layout after the 16-bit `format`/`reserved` fields: `length: u32`,
    /// `language: u32`, `numGroups: u32`, then `numGroups` groups of three
    /// `u32`s each (12 bytes/group).
    fn glyph_id_fmt12(&self, codepoint: u32) -> GlyphId {
        let d = self.data;
        let num_groups = match d.u32_at(12) {
            Some(v) => v as usize,
            None => return GlyphId::NOTDEF,
        };
        let groups_off = 16;

        // Binary search groups by endCharCode (groups are sorted, non-overlapping).
        let mut lo = 0usize;
        let mut hi = num_groups;
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            let base = groups_off + mid * 12;
            let end = d.u32_at(base + 4).unwrap_or(0);
            if codepoint > end {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo >= num_groups {
            return GlyphId::NOTDEF;
        }

        let base = groups_off + lo * 12;
        let start = d.u32_at(base).unwrap_or(u32::MAX);
        if codepoint < start {
            return GlyphId::NOTDEF;
        }
        let start_glyph = d.u32_at(base + 8).unwrap_or(0);
        // Real-world fonts (including Noto Emoji, ~900 glyphs) stay well
        // under 65536 glyphs; GlyphId is u16 throughout this crate (loca /
        // hmtx / glyf all index with it), so truncate rather than widen the
        // type crate-wide for a case that doesn't occur in practice.
        let gid = start_glyph + (codepoint - start);
        if gid > u32::from(u16::MAX) {
            return GlyphId::NOTDEF;
        }
        GlyphId(gid as u16)
    }
}

// ── loca ─────────────────────────────────────────────────────────────────────

/// Glyph offset table — short (format 0) or long (format 1).
pub struct Loca<'a> {
    data: &'a [u8],
    format: i16,
}

impl<'a> Loca<'a> {
    pub fn parse(ff: &FontFile<'a>, format: i16) -> Result<Self, ParseError> {
        Ok(Self {
            data: ff.require(TAG_LOCA, "loca")?,
            format,
        })
    }

    /// Wrap an already-located `loca` table slice.
    #[must_use]
    pub const fn from_bytes(data: &'a [u8], format: i16) -> Self {
        Self { data, format }
    }

    /// Byte offset + length of glyph `gid` within the `glyf` table.
    /// Returns `None` if the glyph has no outline (empty contour).
    #[must_use]
    pub fn glyph_range(&self, gid: GlyphId) -> Option<(usize, usize)> {
        let gid = gid.0 as usize;
        let (start, end) = if self.format == 0 {
            // Short format: 16-bit values, multiply by 2.
            let s = self.data.u16_at(gid * 2)? as usize * 2;
            let e = self.data.u16_at((gid + 1) * 2)? as usize * 2;
            (s, e)
        } else {
            // Long format: 32-bit values.
            let s = self.data.u32_at(gid * 4)? as usize;
            let e = self.data.u32_at((gid + 1) * 4)? as usize;
            (s, e)
        };
        if start == end {
            None
        } else {
            Some((start, end - start))
        }
    }
}

// ── hmtx ─────────────────────────────────────────────────────────────────────

/// Horizontal metrics table.
pub struct Hmtx<'a> {
    data: &'a [u8],
    number_of_hmetrics: u16,
}

impl<'a> Hmtx<'a> {
    pub fn parse(ff: &FontFile<'a>, number_of_hmetrics: u16) -> Result<Self, ParseError> {
        Ok(Self {
            data: ff.require(TAG_HMTX, "hmtx")?,
            number_of_hmetrics,
        })
    }

    /// Wrap an already-located `hmtx` table slice.
    #[must_use]
    pub const fn from_bytes(data: &'a [u8], number_of_hmetrics: u16) -> Self {
        Self {
            data,
            number_of_hmetrics,
        }
    }

    /// Advance width and left side bearing for glyph `gid`.
    #[must_use]
    pub fn metrics(&self, gid: GlyphId) -> (u16, i16) {
        let gid = gid.0 as usize;
        let n = self.number_of_hmetrics as usize;
        if gid < n {
            let base = gid * 4;
            let advance = self.data.u16_at(base).unwrap_or(0);
            let lsb = self.data.i16_at(base + 2).unwrap_or(0);
            (advance, lsb)
        } else {
            // Monospaced tail: advance repeats, only lsb changes.
            let advance = self.data.u16_at((n - 1) * 4).unwrap_or(0);
            let lsb_off = n * 4 + (gid - n) * 2;
            let lsb = self.data.i16_at(lsb_off).unwrap_or(0);
            (advance, lsb)
        }
    }
}
