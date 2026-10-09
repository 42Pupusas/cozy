//! [`Clip`] -- the scanlines and pixel columns a shape fill may touch.

use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Clip {
    pub rows: Range<i32>,
    pub cols: Range<i32>,
}

impl Clip {
    /// Every column, only the scanlines `top..bottom`.
    #[must_use]
    pub const fn rows(top: i32, bottom: i32) -> Self {
        Self {
            rows: top..bottom,
            cols: i32::MIN..i32::MAX,
        }
    }

    /// Only the pixel columns `left..right`, within the scanlines
    /// `top..bottom`.
    #[must_use]
    pub const fn cols(top: i32, bottom: i32, left: i32, right: i32) -> Self {
        Self {
            rows: top..bottom,
            cols: left..right,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Clip;

    #[test]
    fn a_row_clip_leaves_every_column_open() {
        let clip = Clip::rows(3, 9);
        assert_eq!(clip.rows, 3..9);
        assert!(clip.cols.contains(&-1_000_000) && clip.cols.contains(&1_000_000));
    }

    #[test]
    fn a_column_clip_limits_both_axes() {
        let clip = Clip::cols(1, 2, 5, 8);
        assert_eq!((clip.rows, clip.cols), (1..2, 5..8));
    }
}
