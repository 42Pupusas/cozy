//! Vertical placement of terminal rows in a frame.
//!
//! The frame's height is rarely a whole number of cells. Instead of leaving the
//! leftover scanlines as a strip under the last row, they are spread across
//! evenly spaced rows. Row `i` spans `top(i)..top(i + 1)`: `cell_h` glyph
//! scanlines followed by its padding scanlines, and the spans tile the frame
//! exactly.
//!
//! Padding scanlines repeat their row's first glyph scanline, so backgrounds
//! and box-drawing verticals stay continuous while underlines, which sit on the
//! last scanline, keep their thickness. Every painter restores that with
//! [`RowLayout::fill_pad`] after writing a row.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowLayout {
    width: usize,
    height: usize,
    cell_h: usize,
    rows: usize,
    leftover: usize,
}

impl RowLayout {
    #[must_use]
    pub fn new(width: i32, height: i32, cell_h: i32) -> Self {
        let width = usize::try_from(width).unwrap_or(0);
        let height = usize::try_from(height).unwrap_or(0);
        let cell_h = usize::try_from(cell_h).unwrap_or(1).max(1);
        let rows = (height / cell_h).max(1);
        Self {
            width,
            height,
            cell_h,
            rows,
            leftover: height.saturating_sub(rows * cell_h),
        }
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub const fn cell_h(&self) -> usize {
        self.cell_h
    }

    #[must_use]
    pub const fn rows(&self) -> usize {
        self.rows
    }

    /// First scanline of `row`, or the frame bottom for `row == rows()`.
    #[must_use]
    pub const fn boundary(&self, row: usize) -> usize {
        let y = row * self.cell_h + row * self.leftover / self.rows;
        if y < self.height { y } else { self.height }
    }

    /// First scanline of `row`, or `None` past the last row.
    #[must_use]
    pub const fn top(&self, row: usize) -> Option<usize> {
        if row < self.rows {
            Some(self.boundary(row))
        } else {
            None
        }
    }

    /// Every scanline `row` owns, padding included.
    #[must_use]
    pub const fn span(&self, row: usize) -> Option<Range<usize>> {
        if row < self.rows {
            Some(self.boundary(row)..self.boundary(row + 1))
        } else {
            None
        }
    }

    /// The row owning scanline `y`, clamped to the last row.
    #[must_use]
    pub const fn row_at(&self, y: usize) -> usize {
        let mut lo = 0;
        let mut hi = self.rows;
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.boundary(mid) <= y {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The rows owning any scanline of `lines`.
    #[must_use]
    pub const fn rows_in(&self, lines: &Range<usize>) -> Range<usize> {
        let end = if lines.end < self.height {
            lines.end
        } else {
            self.height
        };
        if lines.start >= end {
            return 0..0;
        }
        self.row_at(lines.start)..self.row_at(end - 1) + 1
    }

    /// The scanline a fractional row position maps to, interpolating inside
    /// the row's span. Position `rows()` is the frame bottom; anything past it
    /// is `None`.
    // Row indices and scanline counts are far below f32's exact-integer range.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    #[must_use]
    pub fn y_at(&self, row: f32) -> Option<f32> {
        let row = row.max(0.0);
        let base = row.floor() as usize;
        let frac = row - row.floor();
        if base > self.rows || (base == self.rows && frac > 0.0) {
            return None;
        }
        let start = self.boundary(base) as f32;
        let end = self.boundary((base + 1).min(self.rows)) as f32;
        Some(frac.mul_add(end - start, start))
    }

    const fn pad(&self, row: usize) -> usize {
        self.boundary(row + 1) - self.boundary(row) - self.cell_h
    }

    /// Repeat `row`'s first glyph scanline over its padding scanlines.
    pub fn fill_pad(&self, pixels: &mut [u32], row: usize) {
        self.fill_pad_cols(pixels, row, 0..self.width);
    }

    /// [`Self::fill_pad`] restricted to pixel columns `xs`.
    pub fn fill_pad_cols(&self, pixels: &mut [u32], row: usize, xs: Range<usize>) {
        let Some(span) = self.span(row) else {
            return;
        };
        let w = self.width;
        let xs = xs.start.min(w)..xs.end.min(w);
        let glyph_end = span.start + self.cell_h;
        if xs.is_empty() || glyph_end >= span.end || pixels.len() < span.end * w {
            return;
        }
        let first = span.start * w;
        for line in glyph_end..span.end {
            pixels.copy_within(first + xs.start..first + xs.end, line * w + xs.start);
        }
    }

    /// Move `count` rows starting at `src` so they start at `dst`, as one
    /// memmove per run of rows whose padding matches their destination's.
    pub fn move_rows(&self, pixels: &mut [u32], src: usize, dst: usize, count: usize) {
        let count = count.min(self.rows.saturating_sub(src.max(dst)));
        if count == 0 || src == dst || pixels.len() < self.height * self.width {
            return;
        }
        let same = |i: usize| self.pad(src + i) == self.pad(dst + i);
        if dst < src {
            let mut i = 0;
            while i < count {
                let mut j = i + 1;
                if same(i) {
                    while j < count && same(j) {
                        j += 1;
                    }
                    self.copy_run(pixels, src + i, dst + i, j - i);
                } else {
                    self.copy_row(pixels, src + i, dst + i);
                }
                i = j;
            }
        } else {
            let mut j = count;
            while j > 0 {
                let mut i = j - 1;
                if same(i) {
                    while i > 0 && same(i - 1) {
                        i -= 1;
                    }
                    self.copy_run(pixels, src + i, dst + i, j - i);
                } else {
                    self.copy_row(pixels, src + i, dst + i);
                }
                j = i;
            }
        }
    }

    fn copy_run(&self, pixels: &mut [u32], src: usize, dst: usize, count: usize) {
        let w = self.width;
        let from = self.boundary(src) * w..self.boundary(src + count) * w;
        pixels.copy_within(from, self.boundary(dst) * w);
    }

    fn copy_row(&self, pixels: &mut [u32], src: usize, dst: usize) {
        let w = self.width;
        let from = self.boundary(src) * w;
        pixels.copy_within(from..from + self.cell_h * w, self.boundary(dst) * w);
        self.fill_pad(pixels, dst);
    }

    /// The pixel index range of scanlines `lines` in a row-major frame.
    #[must_use]
    pub const fn pixels(&self, lines: &Range<usize>) -> Range<usize> {
        lines.start * self.width..lines.end * self.width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(layout: &RowLayout) -> Vec<u32> {
        let mut px: Vec<u32> = (0..layout.width * layout.height)
            .map(|i| u32::try_from(i).unwrap_or(0))
            .collect();
        for row in 0..layout.rows() {
            layout.fill_pad(&mut px, row);
        }
        px
    }

    fn move_rowwise(layout: &RowLayout, px: &mut [u32], src: usize, dst: usize, count: usize) {
        let order: Vec<usize> = if dst < src {
            (0..count).collect()
        } else {
            (0..count).rev().collect()
        };
        for i in order {
            layout.copy_row(px, src + i, dst + i);
        }
    }

    #[test]
    fn spans_tile_the_whole_frame_with_near_equal_padding() {
        for height in [10, 37, 40, 799, 800, 2160] {
            let layout = RowLayout::new(3, height, 10);
            assert_eq!(layout.top(0), Some(0));
            assert_eq!(
                layout.span(layout.rows() - 1).map(|s| s.end),
                Some(layout.height())
            );
            let pads: Vec<usize> = (0..layout.rows()).map(|r| layout.pad(r)).collect();
            let lo = pads.iter().min().copied().unwrap_or(0);
            let hi = pads.iter().max().copied().unwrap_or(0);
            assert!(hi - lo <= 1, "height {height}: pads {pads:?}");
        }
    }

    #[test]
    fn leftover_is_spread_one_scanline_per_row() {
        let layout = RowLayout::new(1, 43, 10);
        assert_eq!(layout.rows(), 4);
        let pads: Vec<usize> = (0..4).map(|r| layout.pad(r)).collect();
        assert_eq!(pads.iter().sum::<usize>(), 3);
        assert!(pads.iter().all(|&p| p <= 1));
    }

    #[test]
    fn an_exact_multiple_has_no_padding() {
        let layout = RowLayout::new(1, 40, 10);
        let spans: Vec<_> = (0..4).filter_map(|r| layout.span(r)).collect();
        assert_eq!(spans, vec![0..10, 10..20, 20..30, 30..40]);
    }

    #[test]
    fn a_frame_shorter_than_a_cell_is_one_short_row() {
        let layout = RowLayout::new(1, 7, 10);
        assert_eq!(layout.rows(), 1);
        assert_eq!(layout.span(0), Some(0..7));
    }

    #[test]
    fn rows_past_the_last_have_no_position() {
        let layout = RowLayout::new(1, 43, 10);
        assert_eq!(layout.top(4), None);
        assert_eq!(layout.span(4), None);
        assert_eq!(layout.y_at(4.0), Some(43.0));
        assert_eq!(layout.y_at(4.5), None);
    }

    #[test]
    fn row_at_inverts_span_and_clamps() {
        let layout = RowLayout::new(1, 43, 10);
        for row in 0..layout.rows() {
            for y in layout.span(row).into_iter().flatten() {
                assert_eq!(layout.row_at(y), row);
            }
        }
        assert_eq!(layout.row_at(10_000), layout.rows() - 1);
    }

    #[test]
    fn rows_in_covers_partial_rows_and_stops_at_the_frame() {
        let layout = RowLayout::new(1, 40, 10);
        assert_eq!(layout.rows_in(&(5..25)), 0..3);
        assert_eq!(layout.rows_in(&(35..90)), 3..4);
        assert_eq!(layout.rows_in(&(40..50)), 0..0);
    }

    #[test]
    fn y_at_interpolates_inside_the_span() {
        let layout = RowLayout::new(1, 40, 10);
        assert_eq!(layout.y_at(1.5), Some(15.0));
        assert_eq!(layout.y_at(-2.0), Some(0.0));
    }

    #[test]
    fn fill_pad_repeats_the_first_glyph_scanline() {
        let layout = RowLayout::new(2, 21, 10);
        let mut px: Vec<u32> = (0..42).collect();
        layout.fill_pad(&mut px, 0);
        layout.fill_pad(&mut px, 1);
        let pad_row = (0..2).find(|&r| layout.pad(r) == 1).unwrap_or(0);
        let span = layout.span(pad_row).unwrap_or(0..0);
        let first = span.start * 2;
        let pad = (span.start + 10) * 2;
        assert_eq!(px[pad..pad + 2], px[first..first + 2]);
    }

    #[test]
    fn fill_pad_cols_leaves_other_columns_alone() {
        let layout = RowLayout::new(4, 11, 10);
        let mut px: Vec<u32> = (0..44).collect();
        layout.fill_pad_cols(&mut px, 0, 1..3);
        assert_eq!(px[40..44], [40, 1, 2, 43]);
    }

    #[test]
    fn merged_moves_match_row_by_row_moves_in_both_directions() {
        for height in [40, 43, 47, 400] {
            let layout = RowLayout::new(3, height, 10);
            let rows = layout.rows();
            for delta in 1..rows {
                let keep = rows - delta;
                for (src, dst) in [(delta, 0), (0, delta)] {
                    let mut expected = frame(&layout);
                    move_rowwise(&layout, &mut expected, src, dst, keep);
                    let mut actual = frame(&layout);
                    layout.move_rows(&mut actual, src, dst, keep);
                    assert!(
                        actual == expected,
                        "height {height} src {src} dst {dst} keep {keep}"
                    );
                }
            }
        }
    }

    #[test]
    fn moved_rows_stay_padded_from_their_own_glyphs() {
        let layout = RowLayout::new(2, 47, 10);
        let mut px = frame(&layout);
        layout.move_rows(&mut px, 1, 0, layout.rows() - 1);
        let mut check = px.clone();
        for row in 0..layout.rows() {
            layout.fill_pad(&mut check, row);
        }
        assert_eq!(px, check);
    }

    #[test]
    fn pixels_scale_scanlines_by_width() {
        let layout = RowLayout::new(4, 40, 10);
        assert_eq!(layout.pixels(&(10..30)), 40..120);
    }
}
