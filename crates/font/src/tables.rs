//! Decoded TTF table structs: head, hhea, maxp, cmap (fmt 4), loca, hmtx.

use crate::parse::{
    ByteSliceExt, ParseError,
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
            units_per_em:        d.u16_at(18).ok_or(ParseError::TooShort)?,
            index_to_loc_format: d.i16_at(50).ok_or(ParseError::TooShort)?,
            x_min:               d.i16_at(36).ok_or(ParseError::TooShort)?,
            y_min:               d.i16_at(38).ok_or(ParseError::TooShort)?,
            x_max:               d.i16_at(40).ok_or(ParseError::TooShort)?,
            y_max:               d.i16_at(42).ok_or(ParseError::TooShort)?,
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
            ascender:            d.i16_at(4).ok_or(ParseError::TooShort)?,
            descender:           d.i16_at(6).ok_or(ParseError::TooShort)?,
            line_gap:            d.i16_at(8).ok_or(ParseError::TooShort)?,
            number_of_hmetrics:  d.u16_at(34).ok_or(ParseError::TooShort)?,
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
        let num_tables = d.u16_at(2).ok_or(ParseError::TooShort)? as usize;

        // Walk encoding records looking for a (3,1) or (0,*) format-4 subtable.
        let mut chosen: Option<usize> = None;
        for i in 0..num_tables {
            let base     = 4 + i * 8;
            let platform = d.u16_at(base).ok_or(ParseError::TooShort)?;
            let encoding = d.u16_at(base + 2).ok_or(ParseError::TooShort)?;
            let offset   = d.u32_at(base + 4).ok_or(ParseError::TooShort)? as usize;
            // Prefer Windows/Unicode BMP (3,1), fallback to Unicode (0,*)
            let format = d.u16_at(offset).ok_or(ParseError::TooShort)?;
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
        let seg_count = match d.u16_at(6) {
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
            let end = d.u16_at(end_codes_off + mid * 2).unwrap_or(0);
            if cp > end { lo = mid + 1; } else { hi = mid; }
        }
        if lo >= seg_count { return 0; }

        let start = d.u16_at(start_codes_off + lo * 2).unwrap_or(0xFFFF);
        if cp < start { return 0; }

        let id_range_offset = d.u16_at(id_range_off + lo * 2).unwrap_or(0) as usize;
        if id_range_offset == 0 {
            let delta = i32::from(d.i16_at(id_delta_off + lo * 2).unwrap_or(0));
            let raw = (i32::from(cp) + delta) & 0xFFFF;
            return raw as u16;
        }

        // id_range_offset is relative to its own position in the table.
        let slot_pos = id_range_off + lo * 2;
        let glyph_off = slot_pos + id_range_offset + (cp - start) as usize * 2;
        let glyph_off = glyph_off.saturating_sub(0); // relative to subtable start
        if glyph_off >= glyph_ids_off {
            let idx = glyph_off - glyph_ids_off;
            let raw = d.u16_at(glyph_ids_off + idx).unwrap_or(0);
            if raw == 0 { return 0; }
            let delta = i32::from(d.i16_at(id_delta_off + lo * 2).unwrap_or(0));
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
            let s = self.data.u16_at(gid * 2)? as usize * 2;
            let e = self.data.u16_at((gid + 1) * 2)? as usize * 2;
            (s, e)
        } else {
            // Long format: 32-bit values.
            let s = self.data.u32_at(gid * 4)? as usize;
            let e = self.data.u32_at((gid + 1) * 4)? as usize;
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
            let advance = self.data.u16_at(base).unwrap_or(0);
            let lsb     = self.data.i16_at(base + 2).unwrap_or(0);
            (advance, lsb)
        } else {
            // Monospaced tail: advance repeats, only lsb changes.
            let advance = self.data.u16_at((n - 1) * 4).unwrap_or(0);
            let lsb_off = n * 4 + (gid - n) * 2;
            let lsb     = self.data.i16_at(lsb_off).unwrap_or(0);
            (advance, lsb)
        }
    }
}
