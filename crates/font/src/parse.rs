//! Raw TTF/OTF byte-level helpers: offset table, table directory, big-endian reads.

/// A located table inside the raw font bytes.
#[derive(Debug, Clone, Copy)]
pub struct TableRecord {
    pub offset: usize,
    pub length: usize,
}

/// The parsed font file — just the raw bytes plus the table directory.
#[derive(Debug)]
pub struct FontFile<'a> {
    pub data: &'a [u8],
    pub tables: [Option<TableRecord>; 16],
}

// Table tag indices into `tables`.
pub const TAG_CMAP: usize = 0;
pub const TAG_GLYF: usize = 1;
pub const TAG_HEAD: usize = 2;
pub const TAG_HHEA: usize = 3;
pub const TAG_HMTX: usize = 4;
pub const TAG_LOCA: usize = 5;
pub const TAG_MAXP: usize = 6;
pub const TAG_NAME: usize = 7;
pub const TAG_OS2:  usize = 8;
pub const TAG_POST: usize = 9;
// reserved slots for CFF/CFF2/etc
const _TAG_COUNT: usize = 16;

const fn tag_index(tag: [u8; 4]) -> Option<usize> {
    match &tag {
        b"cmap" => Some(TAG_CMAP),
        b"glyf" => Some(TAG_GLYF),
        b"head" => Some(TAG_HEAD),
        b"hhea" => Some(TAG_HHEA),
        b"hmtx" => Some(TAG_HMTX),
        b"loca" => Some(TAG_LOCA),
        b"maxp" => Some(TAG_MAXP),
        b"name" => Some(TAG_NAME),
        b"OS/2" => Some(TAG_OS2),
        b"post" => Some(TAG_POST),
        _ => None,
    }
}

/// Parse errors.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    BadSfntVersion,
    MissingTable(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort         => write!(f, "font data too short"),
            Self::BadSfntVersion   => write!(f, "not a TTF/OTF file (bad sfnt version)"),
            Self::MissingTable(t)  => write!(f, "required table missing: {t}"),
        }
    }
}

impl std::error::Error for ParseError {}

// ── Big-endian reads ──────────────────────────────────────────────────────────

#[must_use] 
pub fn u8_at(data: &[u8], off: usize) -> Option<u8> {
    data.get(off).copied()
}

#[must_use] 
pub fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

pub fn i16_at(data: &[u8], off: usize) -> Option<i16> {
    u16_at(data, off).map(u16::cast_signed)
}

#[must_use] 
pub fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}



// ── Parser ────────────────────────────────────────────────────────────────────

impl<'a> FontFile<'a> {
    /// Parse the offset table and table directory from raw TTF/OTF bytes.
    pub fn parse(data: &'a [u8]) -> Result<Self, ParseError> {
        if data.len() < 12 {
            return Err(ParseError::TooShort);
        }

        // sfnt version: 0x00010000 = TrueType, 0x4F54544F = 'OTTO' (CFF)
        let sfnt = u32_at(data, 0).ok_or(ParseError::TooShort)?;
        if sfnt != 0x0001_0000 && sfnt != 0x4F54_544F {
            return Err(ParseError::BadSfntVersion);
        }

        let num_tables = u16_at(data, 4).ok_or(ParseError::TooShort)? as usize;
        if data.len() < 12 + num_tables * 16 {
            return Err(ParseError::TooShort);
        }

        let mut tables = [None::<TableRecord>; 16];
        for i in 0..num_tables {
            let base = 12 + i * 16;
            let tag: [u8; 4] = data[base..base + 4].try_into().unwrap();
            let offset = u32_at(data, base + 8).ok_or(ParseError::TooShort)? as usize;
            let length = u32_at(data, base + 12).ok_or(ParseError::TooShort)? as usize;
            if let Some(idx) = tag_index(tag) {
                tables[idx] = Some(TableRecord { offset, length });
            }
        }

        Ok(Self { data, tables })
    }

    #[must_use] 
    pub fn table(&self, idx: usize) -> Option<&'a [u8]> {
        let r = self.tables[idx]?;
        self.data.get(r.offset..r.offset + r.length)
    }

    pub fn require(&self, idx: usize, name: &'static str) -> Result<&'a [u8], ParseError> {
        self.table(idx).ok_or(ParseError::MissingTable(name))
    }
}
