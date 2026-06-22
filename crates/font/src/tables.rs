//! Decoded TTF table structs: head, hhea, maxp, cmap (fmt 4), loca, hmtx.

use crate::parse::{
    ParseError, i16_at, u16_at, u32_at,
    TAG_CMAP, TAG_HEAD, TAG_HHEA, TAG_HMTX, TAG_LOCA, TAG_MAXP,
};
use crate::parse::FontFile;

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
            units_per_em:        u16_at(d, 18).ok_or(ParseError::TooShort)?,
            index_to_loc_format: i16_at(d, 50).ok_or(ParseError::TooShort)?,
            x_min:               i16_at(d, 36).ok_or(ParseError::TooShort)?,
            y_min:               i16_at(d, 38).ok_or(ParseError::TooShort)?,
            x_max:               i16_at(d, 40).ok_or(ParseError::TooShort)?,
            y_max:               i16_at(d, 42).ok_or(ParseError::TooShort)?,
        })
    }
}

// ── hhea ─────────────────────────────────────────────────────────────────────

pub struct Hhea {
    pub ascender:          i16,
    pub descender:         i16,
    pub line_gap:          i16,
    pub number_of_hmetrics: u16,
}

impl Hhea {
    pub fn parse(ff: &FontFile<'_>) -> Result<Self, ParseError> {
        let d = ff.require(TAG_HHEA, "hhea")?;
        Ok(Self {
            ascender:            i16_at(d, 4).ok_or(ParseError::TooShort)?,
            descender:           i16_at(d, 6).ok_or(ParseError::TooShort)?,
            line_gap:            i16_at(d, 8).ok_or(ParseError::TooShort)?,
            number_of_hmetrics:  u16_at(d, 34).ok_or(ParseError::TooShort)?,
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
            num_glyphs: u16_at(d, 4).ok_or(ParseError::TooShort)?,
        })
    }
}

// ── cmap (format 4 — the universal BMP subtable) ─────────────────────────────

/// Map a Unicode codepoint → glyph id.
pub struct Cmap<'a> {
    data: &'a [u8], // the format-4 subtable slice
}

impl<'a> Cmap<'a> {
    pub fn parse(ff: &FontFile<'a>) -> Result<Self, ParseError> {
        let d       = ff.require(TAG_CMAP, "cmap")?;
        let sub_off = Self::find_subtable(d)?;
        Ok(Self { data: &d[sub_off..] })
    }

    /// Wrap an already-located format-4 subtable slice (no directory walk).
    #[must_use] 
    pub const fn from_subtable(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Locate the best format-4 subtable within a whole `cmap` table.
    /// Returns the offset of the subtable relative to the `cmap` table start.
    ///
    /// # Errors
    /// [`ParseError`] if no usable format-4 subtable is present.
    pub fn find_subtable(d: &[u8]) -> Result<usize, ParseError> {
        let num_tables = u16_at(d, 2).ok_or(ParseError::TooShort)? as usize;

        // Walk encoding records looking for a (3,1) or (0,*) format-4 subtable.
        let mut chosen: Option<usize> = None;
        for i in 0..num_tables {
            let base     = 4 + i * 8;
            let platform = u16_at(d, base).ok_or(ParseError::TooShort)?;
            let encoding = u16_at(d, base + 2).ok_or(ParseError::TooShort)?;
            let offset   = u32_at(d, base + 4).ok_or(ParseError::TooShort)? as usize;
            // Prefer Windows/Unicode BMP (3,1), fallback to Unicode (0,*)
            let format = u16_at(d, offset).ok_or(ParseError::TooShort)?;
            if format == 4 && ((platform == 3 && encoding == 1) || platform == 0) {
                chosen = Some(offset);
                if platform == 3 { break; } // best match
            }
        }

        chosen.ok_or(ParseError::MissingTable("cmap format 4"))
    }

    /// Map a Unicode scalar to a glyph index (0 = .notdef).
    #[must_use] 
    pub fn glyph_id(&self, codepoint: u32) -> u16 {
        let d = self.data;
        // format=4, length, language already consumed; seg_count_x2 at offset 6
        let seg_count = match u16_at(d, 6) {
            Some(v) => (v / 2) as usize,
            None    => return 0,
        };

        let end_codes_off   = 14;
        let start_codes_off = 14 + seg_count * 2 + 2; // skip reservedPad
        let id_delta_off    = start_codes_off + seg_count * 2;
        let id_range_off    = id_delta_off    + seg_count * 2;
        let glyph_ids_off   = id_range_off    + seg_count * 2;

        let cp = codepoint as u16;

        // Binary search over end_codes.
        let mut lo = 0usize;
        let mut hi = seg_count;
        while lo < hi {
            let mid = usize::midpoint(lo, hi);
            let end = u16_at(d, end_codes_off + mid * 2).unwrap_or(0);
            if cp > end { lo = mid + 1; } else { hi = mid; }
        }
        if lo >= seg_count { return 0; }

        let start = u16_at(d, start_codes_off + lo * 2).unwrap_or(0xFFFF);
        if cp < start { return 0; }

        let id_range_offset = u16_at(d, id_range_off + lo * 2).unwrap_or(0) as usize;
        if id_range_offset == 0 {
            let delta = i32::from(i16_at(d, id_delta_off + lo * 2).unwrap_or(0));
            let raw = (i32::from(cp) + delta) & 0xFFFF;
            return raw as u16;
        }

        // id_range_offset is relative to its own position in the table.
        let slot_pos = id_range_off + lo * 2;
        let glyph_off = slot_pos + id_range_offset + (cp - start) as usize * 2;
        let glyph_off = glyph_off.saturating_sub(0); // relative to subtable start
        if glyph_off >= glyph_ids_off {
            let idx = glyph_off - glyph_ids_off;
            let raw = u16_at(d, glyph_ids_off + idx).unwrap_or(0);
            if raw == 0 { return 0; }
            let delta = i32::from(i16_at(d, id_delta_off + lo * 2).unwrap_or(0));
            return ((i32::from(raw) + delta) & 0xFFFF) as u16;
        }
        0
    }
}

// ── loca ─────────────────────────────────────────────────────────────────────

/// Glyph offset table — short (format 0) or long (format 1).
pub struct Loca<'a> {
    data:   &'a [u8],
    format: i16,
}

impl<'a> Loca<'a> {
    pub fn parse(ff: &FontFile<'a>, format: i16) -> Result<Self, ParseError> {
        Ok(Self { data: ff.require(TAG_LOCA, "loca")?, format })
    }

    /// Wrap an already-located `loca` table slice.
    #[must_use] 
    pub const fn from_bytes(data: &'a [u8], format: i16) -> Self {
        Self { data, format }
    }

    /// Byte offset + length of glyph `gid` within the `glyf` table.
    /// Returns `None` if the glyph has no outline (empty contour).
    #[must_use] 
    pub fn glyph_range(&self, gid: u16) -> Option<(usize, usize)> {
        let gid = gid as usize;
        let (start, end) = if self.format == 0 {
            // Short format: 16-bit values, multiply by 2.
            let s = u16_at(self.data, gid * 2)? as usize * 2;
            let e = u16_at(self.data, (gid + 1) * 2)? as usize * 2;
            (s, e)
        } else {
            // Long format: 32-bit values.
            let s = u32_at(self.data, gid * 4)? as usize;
            let e = u32_at(self.data, (gid + 1) * 4)? as usize;
            (s, e)
        };
        if start == end { None } else { Some((start, end - start)) }
    }
}

// ── hmtx ─────────────────────────────────────────────────────────────────────

/// Horizontal metrics table.
pub struct Hmtx<'a> {
    data:               &'a [u8],
    number_of_hmetrics: u16,
}

impl<'a> Hmtx<'a> {
    pub fn parse(ff: &FontFile<'a>, number_of_hmetrics: u16) -> Result<Self, ParseError> {
        Ok(Self { data: ff.require(TAG_HMTX, "hmtx")?, number_of_hmetrics })
    }

    /// Wrap an already-located `hmtx` table slice.
    #[must_use] 
    pub const fn from_bytes(data: &'a [u8], number_of_hmetrics: u16) -> Self {
        Self { data, number_of_hmetrics }
    }

    /// Advance width and left side bearing for glyph `gid`.
    #[must_use] 
    pub fn metrics(&self, gid: u16) -> (u16, i16) {
        let gid = gid as usize;
        let n   = self.number_of_hmetrics as usize;
        if gid < n {
            let base    = gid * 4;
            let advance = u16_at(self.data, base).unwrap_or(0);
            let lsb     = i16_at(self.data, base + 2).unwrap_or(0);
            (advance, lsb)
        } else {
            // Monospaced tail: advance repeats, only lsb changes.
            let advance = u16_at(self.data, (n - 1) * 4).unwrap_or(0);
            let lsb_off = n * 4 + (gid - n) * 2;
            let lsb     = i16_at(self.data, lsb_off).unwrap_or(0);
            (advance, lsb)
        }
    }
}
