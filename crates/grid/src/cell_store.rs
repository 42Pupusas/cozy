//! Columnar backing storage for terminal cells.
//!
//! Characters, style ids and placement live in separate planes, so an ASCII
//! flood writes a dense character plane and fills a `u16` id plane with one
//! value. Ids index the owning row's [`RowStyles`] table, which is cleared
//! with the row, so no style table is shared across rows or grows without
//! bound.
//!
//! A cell holding a multi-codepoint grapheme cluster stores a tagged id into
//! its row's [`RowGraphemes`] in the character plane instead of a scalar.

use std::ops::Range;

use crate::row_graphemes::RowGraphemes;
use crate::row_styles::{RowStyles, Style};
use crate::{Attrs, Cell, Cluster, DirtyRowContent};

/// Stores cells as structure-of-arrays planes.
///
/// `chars` is deliberately a `u32` plane rather than Rust `char`: this makes
/// ASCII run writes a simple widening/store kernel and keeps the backing data
/// plain-copyable. Placement is a single byte plane: a cell cannot
/// simultaneously be a wide lead and a spacer.
///
/// The store is `width` cells per row. A range passed to any method lies
/// within one row.
pub(crate) struct CellStore {
    width: usize,
    chars: Vec<u32>,
    style_ids: Vec<u16>,
    row_styles: Vec<RowStyles>,
    row_graphemes: Vec<RowGraphemes>,
    placement: Vec<u8>,
    #[cfg(target_arch = "x86_64")]
    /// Probed once with the store, not once per short terminal-line write.
    avx2: bool,
}

impl CellStore {
    pub(crate) fn new(width: usize, rows: usize) -> Self {
        let len = width * rows;
        Self {
            width,
            chars: vec![u32::from(' '); len],
            style_ids: vec![0; len],
            row_styles: std::iter::repeat_with(RowStyles::default)
                .take(rows)
                .collect(),
            row_graphemes: std::iter::repeat_with(RowGraphemes::default)
                .take(rows)
                .collect(),
            placement: vec![Self::NARROW; len],
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.chars.len()
    }

    /// Bytes one row of `width` cells takes, not counting the heap behind
    /// its style table and cluster pool, which are bounded per row.
    pub(crate) const fn row_bytes(width: usize) -> usize {
        width * (size_of::<u32>() + size_of::<u16>() + size_of::<u8>())
            + size_of::<RowStyles>()
            + size_of::<RowGraphemes>()
    }

    /// Append `rows` blank rows, allocating exactly that much more.
    pub(crate) fn grow(&mut self, rows: usize) {
        let cells = self.chars.len() + rows * self.width;
        let row_count = self.row_styles.len() + rows;
        self.chars.reserve_exact(rows * self.width);
        self.chars.resize(cells, u32::from(' '));
        self.style_ids.reserve_exact(rows * self.width);
        self.style_ids.resize(cells, 0);
        self.placement.reserve_exact(rows * self.width);
        self.placement.resize(cells, Self::NARROW);
        self.row_styles.reserve_exact(rows);
        self.row_styles.resize_with(row_count, RowStyles::default);
        self.row_graphemes.reserve_exact(rows);
        self.row_graphemes.resize_with(row_count, RowGraphemes::default);
    }

    pub(crate) fn cell(&self, index: usize) -> Cell {
        let style = self.row_styles[index / self.width].get(self.style_ids[index]);
        let ch = self.cluster(index).map_or_else(
            || char::from_u32(self.chars[index]).unwrap_or('�'),
            |cluster| cluster[0],
        );
        match self.placement[index] {
            Self::WIDE => Cell::wide_lead(ch, style.attrs, style.link_id),
            Self::SPACER => Cell::wide_spacer(style.attrs, style.link_id),
            _ => Cell::new(ch, style.attrs, style.link_id),
        }
    }

    pub(crate) fn set_cell(&mut self, index: usize, cell: Cell) {
        self.chars[index] = u32::from(cell.ch);
        self.style_ids[index] =
            self.intern(Style::new(cell.attrs, cell.link_id()), index..index + 1);
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
    pub(crate) fn clear_range(&mut self, range: Range<usize>) {
        if range.len() == self.width {
            self.restart_row(range.start);
        }
        self.chars[range.clone()].fill(u32::from(' '));
        self.style_ids[range.clone()].fill(0);
        self.placement[range].fill(Self::NARROW);
    }

    /// Empty the style table of the row holding `index`. Only for a row
    /// whose every cell is about to be cleared or overwritten.
    pub(crate) fn restart_row(&mut self, index: usize) {
        self.row_styles[index / self.width].clear();
        self.row_graphemes[index / self.width].clear();
    }

    /// The codepoints of the cluster at `index`, when it holds more than one.
    pub(crate) fn cluster(&self, index: usize) -> Option<&[char]> {
        let raw = self.chars[index];
        if !RowGraphemes::is_cluster(raw) {
            return None;
        }
        self.row_graphemes[index / self.width].get(raw)
    }

    /// Add `ch` to the cluster at `index`, keeping its style and placement.
    pub(crate) fn join(&mut self, index: usize, ch: char) {
        let mut cluster = self
            .cluster(index)
            .map_or_else(|| vec![self.cell(index).ch], <[char]>::to_vec);
        if cluster.len() < RowGraphemes::MAX_LEN {
            cluster.push(ch);
            self.set_cluster(index, &cluster);
        }
    }

    /// Store `cluster` at `index`, whose style and placement are already
    /// written.
    pub(crate) fn set_cluster(&mut self, index: usize, cluster: &[char]) {
        let cluster = &cluster[..cluster.len().min(RowGraphemes::MAX_LEN)];
        let row = index / self.width;
        let start = row * self.width;
        self.row_graphemes[row].insert(
            cluster,
            &mut self.chars[start..start + self.width],
            index - start,
        );
    }

    /// Write a narrow ASCII run, the flood hot path: widen the bytes into
    /// the character plane and fill the id plane with one value.
    pub(crate) fn write_ascii(&mut self, start: usize, bytes: &[u8], attrs: Attrs, link_id: u32) {
        let end = start + bytes.len();
        let id = self.intern(Style::new(attrs, link_id), start..end);
        self.write_ascii_chars(start, bytes);
        self.style_ids[start..end].fill(id);
        self.placement[start..end].fill(Self::NARROW);
    }

    /// The id of `style` in the row holding `cells`, which the caller is
    /// about to overwrite.
    fn intern(&mut self, style: Style, cells: Range<usize>) -> u16 {
        let row = cells.start / self.width;
        let row_start = row * self.width;
        // Twice the width: a rebuild keeps at most `width - 1` live entries,
        // so it is followed by at least `width` cheap inserts.
        let limit = (2 * self.width).min(usize::from(u16::MAX));
        self.row_styles[row].intern(
            style,
            &mut self.style_ids[row_start..row_start + self.width],
            cells.start - row_start..cells.end - row_start,
            limit,
        )
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
        // SAFETY: SSE2 is unconditionally available on `x86_64` (this crate's
        // only target for the vector path); the callee bounds every vector
        // load/store to the slices it's given.
        unsafe { self.write_ascii_chars_sse2(start + offset, &bytes[offset..]) };
    }

    /// Move cells within one row; the row's style table is unchanged.
    pub(crate) fn copy_within(&mut self, src: Range<usize>, dest: usize) {
        self.chars.copy_within(src.clone(), dest);
        self.style_ids.copy_within(src.clone(), dest);
        self.placement.copy_within(src, dest);
    }

    pub(crate) fn row_content(&self, range: Range<usize>) -> DirtyRowContent {
        let clusters = self.clusters(range.clone());
        if !clusters.is_empty() {
            return DirtyRowContent::Clustered {
                cells: range.map(|index| self.cell(index)).collect(),
                clusters,
            };
        }
        let chars = &self.chars[range.clone()];
        let ids = &self.style_ids[range.clone()];
        let placement = &self.placement[range.clone()];

        // A shell normally emits an entire row under one SGR state. Send that
        // row as codepoints plus one style; mixed SGR or wide rows take the
        // general per-cell path.
        if let Some(&id) = ids.first()
            && placement.iter().all(|&value| value == Self::NARROW)
            && ids.iter().all(|&value| value == id)
        {
            let style = self.row_styles[range.start / self.width].get(id);
            return DirtyRowContent::Uniform {
                chars: chars.to_vec(),
                attrs: style.attrs,
                link_id: style.link_id,
            };
        }

        DirtyRowContent::Cells(range.map(|index| self.cell(index)).collect())
    }

    /// Every multi-codepoint cluster in `range`, columns relative to its start.
    pub(crate) fn clusters(&self, range: Range<usize>) -> Vec<Cluster> {
        if self.row_graphemes[range.start / self.width].is_empty() {
            return Vec::new();
        }
        let start = range.start;
        range
            .filter_map(|index| {
                let col = u16::try_from(index - start).unwrap_or(u16::MAX);
                self.cluster(index)
                    .map(|cluster| Cluster::new(col, cluster))
            })
            .collect()
    }

    pub(crate) fn row_cells(&self, range: Range<usize>) -> Vec<Cell> {
        self.row_content(range).materialize_cells()
    }

    pub(crate) fn cells(&self, range: Range<usize>) -> Vec<Cell> {
        self.row_cells(range)
    }

    /// Normal single-width cell placement.
    const NARROW: u8 = 0;
    /// First cell of a double-width glyph.
    const WIDE: u8 = 1;
    /// Second cell occupied by a double-width glyph.
    const SPACER: u8 = 2;
}

#[cfg(test)]
mod tests {
    use super::CellStore;
    use crate::{Attrs, Cell, Color, DirtyRowContent};

    #[test]
    fn ascii_widening_preserves_each_byte_across_vector_and_scalar_tails() {
        let bytes = b"0123456789abcdef0123456789abcdef!";
        let mut store = CellStore::new(bytes.len(), 1);
        store.write_ascii(0, bytes, Attrs::default(), 0);
        for (index, &byte) in bytes.iter().enumerate() {
            assert_eq!(store.cell(index).ch, char::from(byte));
        }
    }

    #[test]
    fn row_cells_preserves_uniform_ascii_style() {
        let mut store = CellStore::new(4, 1);
        let attrs = Attrs::with_colors(Color::Indexed(1), Color::Rgb(2, 3, 4));
        store.write_ascii(0, b"test", attrs, 7);

        let row = store.row_content(0..4);
        assert!(matches!(row, DirtyRowContent::Uniform { .. }));
        let cells = row.materialize_cells();
        assert_eq!(cells[0], Cell::new('t', attrs, 7));
        assert_eq!(cells[3], Cell::new('t', attrs, 7));
    }

    fn color(i: usize) -> Color {
        let [.., r, g, b] = i.to_be_bytes();
        Color::Rgb(r, g, b)
    }

    #[test]
    fn more_distinct_styles_than_u16_keep_their_own_colors() {
        let count = 80 * 1000;
        let mut store = CellStore::new(80, 1000);
        for i in 0..count {
            store.write_ascii(i, b"#", Attrs::with_colors(color(i), Color::Default), 0);
        }
        for i in 0..count {
            assert_eq!(store.cell(i).attrs.fg, color(i), "cell {i}");
        }
    }

    #[test]
    fn repainting_one_row_in_new_colors_never_corrupts_it() {
        let mut store = CellStore::new(8, 1);
        for frame in 0..10_000 {
            for col in 0..8 {
                let attrs = Attrs::with_colors(color(frame * 8 + col), Color::Default);
                store.write_ascii(col, b"x", attrs, 0);
            }
        }
        for col in 0..8 {
            assert_eq!(store.cell(col).attrs.fg, color(9_999 * 8 + col));
        }
    }

    #[test]
    fn clusters_clear_with_their_row() {
        let mut store = CellStore::new(4, 2);
        store.write_ascii(4, b"abcd", Attrs::default(), 0);
        store.join(5, '\u{301}');
        store.join(5, '\u{323}');
        assert_eq!(store.cell(5).ch, 'b');
        assert_eq!(
            store.row_content(4..8).clusters(),
            [crate::Cluster::new(1, &['b', '\u{301}', '\u{323}'])]
        );
        store.clear_range(4..8);
        assert_eq!(store.cluster(5), None);
    }

    #[test]
    fn row_cells_preserves_mixed_placement_and_styles() {
        let mut store = CellStore::new(4, 1);
        let attrs = Attrs::with_colors(Color::Indexed(1), Color::Rgb(2, 3, 4));
        store.set_cell(0, Cell::wide_lead('全', attrs, 7));
        store.set_cell(1, Cell::wide_spacer(attrs, 7));
        store.set_cell(2, Cell::new('x', attrs, 7));
        store.copy_within(0..3, 1);

        let row = store.row_content(1..4);
        assert!(matches!(row, DirtyRowContent::Cells(_)));
        let cells = row.materialize_cells();
        assert!(cells[0].wide());
        assert!(cells[1].spacer());
        assert_eq!(cells[2].ch, 'x');
    }
}
