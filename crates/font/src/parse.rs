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
pub const TAG_OS2: usize = 8;
pub const TAG_POST: usize = 9;
// reserved slots for CFF/CFF2/etc
const _TAG_COUNT: usize = 16;

/// The `ttcf` magic (`TrueType` Collection header), big-endian as a `u32` --
/// i.e. `Self::parse` sees this instead of a normal sfnt version (`0x00010000`
/// / `OTTO`) at byte 0 when the file is a `.ttc` bundling several sub-fonts.
const TTC_TAG: u32 = 0x7474_6366; // b"ttcf"

/// A raw 4-byte sfnt table tag (`"cmap"`, `"glyf"`, ...) as found in a font's
/// table directory.
///
/// Was a free `tag_index(tag: [u8; 4]) -> Option<usize>` function; its sole
/// argument is a bare `[u8; 4]` with no type of its own to hang the lookup
/// off of, so this newtype gives it one -- `Tag(raw).index()` reads as
/// "this tag's slot", rather than a disembodied byte-array classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tag(pub [u8; 4]);

impl Tag {
    /// This tag's slot in [`FontFile::tables`], or `None` for tags this
    /// parser doesn't track (CFF/CFF2/etc).
    const fn index(self) -> Option<usize> {
        match &self.0 {
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
}

/// Parse errors.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    BadSfntVersion,
    MissingTable(&'static str),
    /// A `ttcf` (`TrueType` Collection) header claimed zero fonts -- a
    /// well-formed TTC always has `numFonts >= 1`; this is corrupt/truncated
    /// data, not a font this parser simply doesn't support.
    EmptyCollection,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "font data too short"),
            Self::BadSfntVersion => write!(f, "not a TTF/OTF file (bad sfnt version)"),
            Self::MissingTable(t) => write!(f, "required table missing: {t}"),
            Self::EmptyCollection => write!(f, "TrueType collection (ttcf) has zero fonts"),
        }
    }
}

impl std::error::Error for ParseError {}

// ── Big-endian reads ──────────────────────────────────────────────────────────

/// Big-endian scalar reads off a raw byte slice.
///
/// Methods on `&[u8]` instead of free functions that all took `data: &[u8]`
/// as their first argument — every TTF/OTF table walk in this crate wants
/// `slice.u16_at(off)`, not a namespaced helper repeating "reads from a
/// slice" at each call site.
#[must_use]
pub trait ByteSliceExt {
    /// Read a `u8` at `off`, or `None` if out of range.
    fn u8_at(&self, off: usize) -> Option<u8>;
    /// Read a big-endian `u16` at `off`, or `None` if out of range.
    fn u16_at(&self, off: usize) -> Option<u16>;
    /// Read a big-endian `i16` at `off`, or `None` if out of range.
    fn i16_at(&self, off: usize) -> Option<i16>;
    /// Read a big-endian `u32` at `off`, or `None` if out of range.
    fn u32_at(&self, off: usize) -> Option<u32>;
}

impl ByteSliceExt for [u8] {
    fn u8_at(&self, off: usize) -> Option<u8> {
        self.get(off).copied()
    }

    fn u16_at(&self, off: usize) -> Option<u16> {
        let b = self.get(off..off + 2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16_at(&self, off: usize) -> Option<i16> {
        self.u16_at(off).map(u16::cast_signed)
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        let b = self.get(off..off + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

impl<'a> FontFile<'a> {
    /// Parse the offset table and table directory from raw TTF/OTF/TTC bytes.
    ///
    /// Transparently unwraps a `ttcf` (`TrueType` Collection, e.g. Noto's
    /// `NotoSansCJK-Regular.ttc`) by reading its header's first font offset
    /// and parsing the sfnt table directory found there -- i.e. this always
    /// loads the *first* face in a collection. `.ttc` files bundle several
    /// weights/scripts of one family behind one `ttcf` wrapper purely to
    /// dedupe shared glyph outlines on disk; sub-font 0 is a real, complete,
    /// independently-usable font like any other -- there's no "combined"
    /// font to pick instead.
    pub fn parse(data: &'a [u8]) -> Result<Self, ParseError> {
        if data.len() < 12 {
            return Err(ParseError::TooShort);
        }
        let sfnt = data.u32_at(0).ok_or(ParseError::TooShort)?;
        if sfnt == TTC_TAG {
            let num_fonts = data.u32_at(8).ok_or(ParseError::TooShort)?;
            if num_fonts == 0 {
                return Err(ParseError::EmptyCollection);
            }
            // TableDirectory offsets array starts right after the 12-byte
            // TTC header (version-independent: v1 and v2 share this prefix).
            let sub_offset = data.u32_at(12).ok_or(ParseError::TooShort)? as usize;
            return Self::parse_sfnt_at(data, sub_offset);
        }
        Self::parse_sfnt_at(data, 0)
    }

    /// Parse one sfnt table directory (offset table + records) starting at
    /// byte `base` in `data` -- `0` for a bare TTF/OTF, or a sub-font's
    /// offset for a `ttcf` collection (see [`Self::parse`]). Table record
    /// offsets in the directory are absolute (from the start of `data`),
    /// same in both cases, so nothing downstream needs to know which case
    /// it came from.
    fn parse_sfnt_at(data: &'a [u8], base: usize) -> Result<Self, ParseError> {
        let sfnt = data.u32_at(base).ok_or(ParseError::TooShort)?;
        // sfnt version: 0x00010000 = TrueType, 0x4F54544F = 'OTTO' (CFF)
        if sfnt != 0x0001_0000 && sfnt != 0x4F54_544F {
            return Err(ParseError::BadSfntVersion);
        }

        let num_tables = data.u16_at(base + 4).ok_or(ParseError::TooShort)? as usize;
        if data.len() < base + 12 + num_tables * 16 {
            return Err(ParseError::TooShort);
        }

        let mut tables = [None::<TableRecord>; 16];
        for i in 0..num_tables {
            let rec = base + 12 + i * 16;
            let tag: [u8; 4] = data[rec..rec + 4].try_into().unwrap();
            let offset = data.u32_at(rec + 8).ok_or(ParseError::TooShort)? as usize;
            let length = data.u32_at(rec + 12).ok_or(ParseError::TooShort)? as usize;
            if let Some(idx) = Tag(tag).index() {
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
