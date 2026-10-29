//! Cell width of any code point by one table read, two bits per code point.
//! The data lives in `width_table_data.rs`, written by
//! `examples/gen_width_table.rs`.

use crate::codepoint::CodePoint;

/// Two-level lookup: `INDEX` picks a 64-byte block for each run of 256 code
/// points, and the block holds their widths.
pub struct WidthTable;

impl WidthTable {
    #[inline]
    pub const fn width(cp: CodePoint) -> u8 {
        let CodePoint(cp) = cp;
        let cp = cp as usize;
        if cp >= Self::INDEX.len() * 256 {
            return 1;
        }
        let block = Self::INDEX[cp >> 8] as usize;
        let byte = Self::BLOCKS[block][(cp & 0xff) >> 2];
        (byte >> ((cp & 3) * 2)) & 3
    }
}
