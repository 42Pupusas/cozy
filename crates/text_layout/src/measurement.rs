//! Glyph-width measurement via Unicode UAX #11 East Asian Width.
//!
//! Replaces `measurement.ts` / `Canvas.measureText()` from pretext.
//!
//! In a fixed-width terminal every glyph is either:
//! - **1 cell wide** — most Latin, Greek, Cyrillic, Arabic, Hebrew, etc.
//! - **2 cells wide** — CJK ideographs, fullwidth forms, many emoji
//! - **0 cells wide** — combining marks, zero-width joiners, variation selectors
//!
//! Widths come from [`WidthTable`], generated from the Unicode Character
//! Database by `examples/gen_width_table.rs`.
//!
//! For GUI chrome (non-terminal surfaces) where pixel-accurate advance widths
//! matter, callers should multiply cell widths by the cell pixel width.

use crate::codepoint::CodePoint;
use crate::width_table::WidthTable;

/// Cell-width measurement, as a method on `char`.
///
/// Instead of a free function that took `char` as its first argument -- the
/// same shape as the `unicode-width` crate's `UnicodeWidthChar` trait, which
/// this mirrors for our terminal-specific width rules.
pub trait CellWidth {
    /// The cell width of `self`: 0, 1, or 2.
    fn cell_width(&self) -> u8;
}

impl CellWidth for char {
    /// - `0` — combining / zero-width characters (do not advance the cursor)
    /// - `1` — narrow characters (the default for most of Unicode)
    /// - `2` — wide characters (CJK, fullwidth, certain emoji)
    ///
    /// ASCII printable is answered by one compare, so plain text never
    /// touches the table.
    #[inline]
    fn cell_width(&self) -> u8 {
        let cp = *self as u32;
        if (0x20..=0x7e).contains(&cp) {
            return 1;
        }
        WidthTable::width(CodePoint(cp))
    }
}

/// Extends `str` with total cell-width measurement (sums each char's
/// [`CellWidth::cell_width`]). A separate trait from `CellWidth` since the
/// return type differs (`usize` total vs. `u8` per-char).
pub trait StrCellWidth {
    /// The total cell width of every char in `self`.
    fn str_width(&self) -> usize;
}

impl StrCellWidth for str {
    fn str_width(&self) -> usize {
        self.chars().map(|c| usize::from(c.cell_width())).sum()
    }
}

/// Extends `str` with a `segment_width` alias for [`StrCellWidth::str_width`],
/// named separately to mirror pretext's API surface (a "segment" is just a
/// `&str` slice from the segmenter's point of view).
pub trait SegmentCellWidth {
    /// The cell width of `self` when treated as a layout segment.
    fn segment_width(&self) -> usize;
}

impl SegmentCellWidth for str {
    #[inline]
    fn segment_width(&self) -> usize {
        self.str_width()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_1() {
        for c in ' '..='~' {
            assert_eq!(c.cell_width(), 1, "expected width 1 for {c:?}");
        }
    }

    #[test]
    fn controls_are_0() {
        assert_eq!('\0'.cell_width(), 0);
        assert_eq!('\x1b'.cell_width(), 0);
        assert_eq!('\x7f'.cell_width(), 0);
        assert_eq!('\u{9f}'.cell_width(), 0);
    }

    #[test]
    fn cjk_is_2() {
        assert_eq!('中'.cell_width(), 2);
        assert_eq!('あ'.cell_width(), 2);
        assert_eq!('한'.cell_width(), 2);
        assert_eq!('\u{20000}'.cell_width(), 2);
    }

    #[test]
    fn latin_extended_is_1() {
        assert_eq!('é'.cell_width(), 1);
        assert_eq!('ñ'.cell_width(), 1);
        assert_eq!('ü'.cell_width(), 1);
    }

    #[test]
    fn zwj_is_0() {
        assert_eq!('\u{200d}'.cell_width(), 0); // ZWJ
        assert_eq!('\u{200b}'.cell_width(), 0); // ZWSP
    }

    /// Combining marks past U+09FF, which the old hand-written table missed.
    #[test]
    fn later_combining_marks_are_0() {
        assert_eq!('\u{0e31}'.cell_width(), 0); // Thai mai han-akat
        assert_eq!('\u{0f71}'.cell_width(), 0); // Tibetan vowel sign aa
        assert_eq!('\u{102d}'.cell_width(), 0); // Myanmar vowel sign i
        assert_eq!('\u{20e3}'.cell_width(), 0); // combining enclosing keycap
        assert_eq!('\u{fe0f}'.cell_width(), 0); // variation selector 16
        assert_eq!('\u{e0061}'.cell_width(), 0); // tag latin small a
    }

    #[test]
    fn emoji_presentation_is_2() {
        assert_eq!('\u{1f600}'.cell_width(), 2);
        assert_eq!('\u{1f1fa}'.cell_width(), 2);
        assert_eq!('\u{231a}'.cell_width(), 2); // watch
    }

    #[test]
    fn spacing_marks_and_unassigned_are_1() {
        assert_eq!('\u{093f}'.cell_width(), 1); // Devanagari vowel sign i (Mc)
        assert_eq!('\u{e0080}'.cell_width(), 1);
        assert_eq!(char::MAX.cell_width(), 1);
    }

    #[test]
    fn str_width_mixed() {
        assert_eq!("hello".str_width(), 5);
        assert_eq!("中文".str_width(), 4);
        assert_eq!("hi中".str_width(), 4);
        assert_eq!("".str_width(), 0);
    }
}
