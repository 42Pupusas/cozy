//! Columnar backing storage for terminal cells.
//!
//! A terminal flood changes a contiguous stream of characters while its SGR
//! state normally stays constant for thousands of cells. Keeping the complete
//! `Cell` value in every slot forced the hot path to write 20 bytes for every
//! incoming ASCII byte. `CellStore` separates the rapidly-changing character
//! plane from an interned style plane: common ASCII output writes four bytes
//! per cell, while attributes/link/placement are reconstructed only when a
//! consumer needs a `Cell` value (render snapshots, selection, resize).

use crate::{Attrs, Cell};

/// One interned render style. Placement remains per-cell because a wide lead
/// and its spacer share SGR/link state but have different geometry.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    attrs: Attrs,
    link_id: u32,
}

impl Style {
    const DEFAULT: Self = Self {
        attrs: Attrs::with_colors(crate::Color::Default, crate::Color::Default),
        link_id: 0,
    };

    const fn from_cell(cell: Cell) -> Self {
        Self {
            attrs: cell.attrs.without_placement(),
            link_id: cell.link_id(),
        }
    }
}

/// Stores cells as structure-of-arrays planes.
///
/// `chars` is deliberately a `u32` plane rather than Rust `char`: this makes
/// ASCII run writes a simple widening/store kernel and keeps the backing data
/// plain-copyable. `style_ids` indexes the per-screen style table. Placement
/// is a single byte plane: a cell cannot simultaneously be a wide lead and a
/// spacer, so separate bit planes only made the ASCII writer clear redundant
/// memory.
pub(crate) struct CellStore {
    chars: Vec<u32>,
    style_ids: Vec<u16>,
    placement: Vec<u8>,
    #[cfg(target_arch = "x86_64")]
    /// Probed once with the store, not once per short terminal-line write.
    avx2: bool,
    styles: Vec<Style>,
    /// The overwhelming common case is a long stream with unchanged SGR and
    /// hyperlink state. Remember its id so an ASCII run does not linearly
    /// search even the small style table on every wrapped row.
    cached_style: Style,
    cached_id: u16,
}

impl CellStore {
    pub(crate) fn new(len: usize) -> Self {
        Self {
            chars: vec![u32::from(' '); len],
            style_ids: vec![0; len],
            placement: vec![Self::NARROW; len],
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
            styles: vec![Style::DEFAULT],
            cached_style: Style::DEFAULT,
            cached_id: 0,
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.chars.len()
    }

    pub(crate) fn cell(&self, index: usize) -> Cell {
        let style = self.styles[usize::from(self.style_ids[index])];
        let ch = char::from_u32(self.chars[index]).unwrap_or('�');
        let mut cell = Cell::new(ch, style.attrs, style.link_id);
        if self.placement[index] == Self::WIDE {
            cell = Cell::wide_lead(ch, style.attrs, style.link_id);
        } else if self.placement[index] == Self::SPACER {
            cell = Cell::wide_spacer(style.attrs, style.link_id);
        }
        cell
    }

    pub(crate) fn set_cell(&mut self, index: usize, cell: Cell) {
        let style_id = self.intern(Style::from_cell(cell));
        self.chars[index] = u32::from(cell.ch);
        self.style_ids[index] = style_id;
        self.placement[index] = if cell.wide() {
            Self::WIDE
        } else if cell.spacer() {
            Self::SPACER
        } else {
            Self::NARROW
        };
    }

    /// Fill the range with real default cells. Used only when a lazily blank
    /// row must be partially edited; normal full-width output avoids it.
    pub(crate) fn clear_range(&mut self, range: std::ops::Range<usize>) {
        self.chars[range.clone()].fill(u32::from(' '));
        self.style_ids[range.clone()].fill(0);
        self.placement[range].fill(Self::NARROW);
    }

    /// Write a narrow ASCII run. It is the flood hot path: attributes/link
    /// are interned once, then only character words and one uniform style-id
    /// plane are written. The byte widening belongs to this storage owner;
    /// its scalar implementation is intentionally simple before adding a
    /// measured SIMD kernel over the contiguous `chars` plane.
    pub(crate) fn write_ascii(&mut self, start: usize, bytes: &[u8], attrs: Attrs, link_id: u32) {
        let end = start + bytes.len();
        let style_id = self.intern(Style {
            attrs: attrs.without_placement(),
            link_id,
        });
        self.write_ascii_chars(start, bytes);
        self.style_ids[start..end].fill(style_id);
        self.placement[start..end].fill(Self::NARROW);
    }

    /// Widen ASCII bytes into the Unicode scalar-value plane.
    ///
    /// The parser guarantees this is printable ASCII, so every byte maps
    /// directly to the same Unicode scalar. On `x86_64`, widening sixteen bytes
    /// at a time avoids scalar loop overhead while preserving the portable
    /// scalar implementation for other architectures.
    #[cfg(target_arch = "x86_64")]
    fn write_ascii_chars(&mut self, start: usize, bytes: &[u8]) {
        // The CPU capability is captured once in `new`; the per-run branch is
        // stable for the life of the store and predicts perfectly.
        if self.avx2 {
            // SAFETY: `avx2` was set by the runtime capability probe above.
            unsafe { self.write_ascii_chars_avx2(start, bytes) };
        } else {
            // SAFETY: x86_64 guarantees SSE2. The callee bounds every vector
            // load and store to its supplied source/destination slices.
            unsafe { self.write_ascii_chars_sse2(start, bytes) };
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn write_ascii_chars(&mut self, start: usize, bytes: &[u8]) {
        for (dst, &byte) in self.chars[start..start + bytes.len()].iter_mut().zip(bytes) {
            *dst = u32::from(byte);
        }
    }

    /// SSE2 widening kernel for the contiguous ASCII character plane.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "sse2")]
    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn write_ascii_chars_sse2(&mut self, start: usize, bytes: &[u8]) {
        use std::arch::x86_64::{
            __m128i, _mm_loadu_si128, _mm_setzero_si128, _mm_srli_si128, _mm_storeu_si128,
            _mm_unpacklo_epi8, _mm_unpacklo_epi16,
        };

        let mut offset = 0;
        let destination = self.chars[start..start + bytes.len()].as_mut_ptr();
        while offset + 16 <= bytes.len() {
            // SAFETY: the loop condition keeps the 16-byte source load and
            // four 4-word destination stores within their respective slices;
            // `loadu`/`storeu` accept arbitrary alignment.
            unsafe {
                let source = _mm_loadu_si128(bytes.as_ptr().add(offset).cast::<__m128i>());
                let zero = _mm_setzero_si128();
                let words_lo = _mm_unpacklo_epi8(source, zero);
                let words_hi = _mm_unpacklo_epi8(_mm_srli_si128(source, 8), zero);
                _mm_storeu_si128(
                    destination.add(offset).cast::<__m128i>(),
                    _mm_unpacklo_epi16(words_lo, zero),
                );
                _mm_storeu_si128(
                    destination.add(offset + 4).cast::<__m128i>(),
                    _mm_unpacklo_epi16(_mm_srli_si128(words_lo, 8), zero),
                );
                _mm_storeu_si128(
                    destination.add(offset + 8).cast::<__m128i>(),
                    _mm_unpacklo_epi16(words_hi, zero),
                );
                _mm_storeu_si128(
                    destination.add(offset + 12).cast::<__m128i>(),
                    _mm_unpacklo_epi16(_mm_srli_si128(words_hi, 8), zero),
                );
            }
            offset += 16;
        }
        for (dst, &byte) in self.chars[start + offset..start + bytes.len()]
            .iter_mut()
            .zip(&bytes[offset..])
        {
            *dst = u32::from(byte);
        }
    }

    /// AVX2 widening kernel. The 256-bit form widens 32 bytes per iteration,
    /// halving the loop/control overhead versus the baseline SSE2 kernel.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn write_ascii_chars_avx2(&mut self, start: usize, bytes: &[u8]) {
        use std::arch::x86_64::{
            __m128i, __m256i, _mm_srli_si128, _mm256_castsi256_si128, _mm256_cvtepu8_epi32,
            _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_storeu_si256,
        };

        let mut offset = 0;
        let destination = self.chars[start..start + bytes.len()].as_mut_ptr();
        while offset + 32 <= bytes.len() {
            // SAFETY: the loop condition keeps the source load and all four
            // 8-word destination stores in-bounds; unaligned vector access is
            // explicitly supported by these intrinsics.
            unsafe {
                let source = _mm256_loadu_si256(bytes.as_ptr().add(offset).cast::<__m256i>());
                let low: __m128i = _mm256_castsi256_si128(source);
                let high: __m128i = _mm256_extracti128_si256(source, 1);
                _mm256_storeu_si256(
                    destination.add(offset).cast::<__m256i>(),
                    _mm256_cvtepu8_epi32(low),
                );
                _mm256_storeu_si256(
                    destination.add(offset + 8).cast::<__m256i>(),
                    _mm256_cvtepu8_epi32(_mm_srli_si128(low, 8)),
                );
                _mm256_storeu_si256(
                    destination.add(offset + 16).cast::<__m256i>(),
                    _mm256_cvtepu8_epi32(high),
                );
                _mm256_storeu_si256(
                    destination.add(offset + 24).cast::<__m256i>(),
                    _mm256_cvtepu8_epi32(_mm_srli_si128(high, 8)),
                );
            }
            offset += 32;
        }
        // The SSE2 kernel efficiently handles the 16-byte block plus scalar
        // tail that remains after the AVX2 bulk loop.
        unsafe { self.write_ascii_chars_sse2(start + offset, &bytes[offset..]) };
    }

    pub(crate) fn copy_within(&mut self, src: std::ops::Range<usize>, dest: usize) {
        self.chars.copy_within(src.clone(), dest);
        self.style_ids.copy_within(src.clone(), dest);
        self.placement.copy_within(src, dest);
    }

    pub(crate) fn row_cells(&self, range: std::ops::Range<usize>) -> Vec<Cell> {
        range.map(|index| self.cell(index)).collect()
    }

    pub(crate) fn cells(&self, range: std::ops::Range<usize>) -> Vec<Cell> {
        self.row_cells(range)
    }

    /// Normal single-width cell placement.
    const NARROW: u8 = 0;
    /// First cell of a double-width glyph.
    const WIDE: u8 = 1;
    /// Second cell occupied by a double-width glyph.
    const SPACER: u8 = 2;

    fn intern(&mut self, style: Style) -> u16 {
        if style == self.cached_style {
            return self.cached_id;
        }
        let id = if let Some(index) = self.styles.iter().position(|known| *known == style) {
            u16::try_from(index).expect("style table is bounded to u16")
        } else {
            // A per-screen style table of 65k distinct SGR/link combinations
            // is already pathological. Degrade to the default style rather
            // than aliasing an unrelated one if a hostile stream reaches it.
            let Ok(index) = u16::try_from(self.styles.len()) else {
                return 0;
            };
            self.styles.push(style);
            index
        };
        self.cached_style = style;
        self.cached_id = id;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::CellStore;
    use crate::{Attrs, Cell, Color};

    #[test]
    fn ascii_widening_preserves_each_byte_across_vector_and_scalar_tails() {
        let bytes = b"0123456789abcdef0123456789abcdef!";
        let mut store = CellStore::new(bytes.len());
        store.write_ascii(0, bytes, Attrs::default(), 0);
        for (index, &byte) in bytes.iter().enumerate() {
            assert_eq!(store.cell(index).ch, char::from(byte));
        }
    }

    #[test]
    fn round_trips_cells_and_plane_copies() {
        let mut store = CellStore::new(4);
        let attrs = Attrs::with_colors(Color::Indexed(1), Color::Rgb(2, 3, 4));
        store.set_cell(0, Cell::wide_lead('全', attrs, 7));
        store.set_cell(1, Cell::wide_spacer(attrs, 7));
        store.set_cell(2, Cell::new('x', attrs, 7));
        store.copy_within(0..3, 1);
        assert!(store.cell(1).wide());
        assert!(store.cell(2).spacer());
        assert_eq!(store.cell(3).ch, 'x');
    }
}
