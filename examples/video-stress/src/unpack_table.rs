//! Byte-to-eight-pixels expansion used by every unpack path.
//!
//! Unpacking one bit at a time costs a shift, a mask, and a branch per pixel.
//! Expanding a whole byte through a 256-entry table replaces that with one
//! indexed load plus an eight-element copy, which the compiler turns into two
//! aligned word stores for the XRGB case.

/// A precomputed expansion of every possible packed byte.
pub struct UnpackTable<T> {
    entries: [[T; 8]; 256],
}

impl<T: Copy> UnpackTable<T> {
    /// Build the table from the two values a single bit selects.
    #[must_use]
    pub const fn new(zero: T, one: T) -> Self {
        let mut entries = [[zero; 8]; 256];
        let mut byte = 0usize;
        while byte < 256 {
            let mut bit = 0usize;
            while bit < 8 {
                if byte >> (7 - bit) & 1 == 1 {
                    entries[byte][bit] = one;
                }
                bit += 1;
            }
            byte += 1;
        }
        Self { entries }
    }

    /// The eight pixels encoded by one packed byte, most significant bit
    /// leftmost.
    #[must_use]
    pub const fn expand(&self, byte: u8) -> &[T; 8] {
        &self.entries[byte as usize]
    }

    /// Expand `packed` into `out`, stopping at `width` pixels per row so the
    /// padding bits of a row's final byte never reach the destination.
    pub fn expand_row(&self, packed: &[u8], width: usize, out: &mut [T]) {
        let mut written = 0usize;
        for &byte in packed {
            let remaining = width - written;
            if remaining == 0 {
                break;
            }
            let run = remaining.min(8);
            out[written..written + run].copy_from_slice(&self.expand(byte)[..run]);
            written += run;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UnpackTable;

    #[test]
    fn expands_bits_most_significant_first() {
        let table = UnpackTable::new(0u32, 1u32);
        assert_eq!(*table.expand(0b1000_0001), [1, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(*table.expand(0), [0; 8]);
        assert_eq!(*table.expand(0xFF), [1; 8]);
    }

    #[test]
    fn row_expansion_ignores_padding_bits() {
        let table = UnpackTable::new(0u8, 1u8);
        let mut out = [0u8; 13];
        table.expand_row(&[0xFF, 0xFF], 13, &mut out);
        assert_eq!(out, [1u8; 13]);
    }

    #[test]
    fn row_expansion_writes_every_pixel_once() {
        let table = UnpackTable::new(7u8, 9u8);
        let mut out = [0u8; 8];
        table.expand_row(&[0b1010_1010], 8, &mut out);
        assert_eq!(out, [9, 7, 9, 7, 9, 7, 9, 7]);
    }
}
