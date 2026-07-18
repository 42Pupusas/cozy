//! Public layout API — port of `layout.ts` from pretext.
//!
//! Ties segmentation, measurement, `BiDi` detection, and line-breaking together
//! into a single `Layouter::layout` call that returns a `Vec<LayoutLine>`.

use crate::analysis::Segmenter;
use crate::bidi::{BidiStr, Direction};
use crate::line_break::LineBreaker;

/// Parameters for a layout call.
#[derive(Copy, Clone, Debug, Default)]
pub struct LayoutParams {
    /// Maximum line width in cells.  `0` = unlimited (single line).
    pub max_width: usize,
    /// Override base paragraph direction.  `None` = auto-detect (UAX #9 P2–P3).
    pub direction: Option<Direction>,
}

/// A single laid-out line.
#[derive(Clone, Debug)]
pub struct LayoutLine<'a> {
    /// The original text slice that makes up this line (may span multiple segments).
    pub text: &'a str,
    /// Total cell width of the line.
    pub width: usize,
    /// Resolved base direction for this line.
    pub direction: Direction,
    /// 0-based line index.
    pub index: usize,
}

/// Stateless orchestrator tying [`Segmenter`], [`crate::bidi`], and
/// [`LineBreaker`] together into the public layout entry point.
///
/// A struct (rather than a bare function) so the call reads as
/// `Layouter.layout(...)`, matching its collaborators' shape.
pub struct Layouter;

impl Layouter {
    /// Lay out `text` according to `params`.
    ///
    /// Returns one [`LayoutLine`] per visual line.  Lines that result from
    /// mandatory breaks (newlines) will have `width == 0` if they are empty;
    /// the last line always appears even if empty.
    #[must_use]
    pub fn layout(text: &str, params: LayoutParams) -> Vec<LayoutLine<'_>> {
        let dir = params
            .direction
            .unwrap_or_else(|| text.paragraph_direction());
        let segments = Segmenter::segment(text);
        let breaks = LineBreaker::compute_breaks(&segments, params.max_width);

        let mut lines = Vec::with_capacity(breaks.len());

        for (idx, lb) in breaks.iter().enumerate() {
            // Reconstruct the text slice for this line by spanning from the first
            // segment's start to the last segment's end within the break range.
            let slice = if lb.start_segment >= lb.end_segment {
                ""
            } else {
                let first = segments[lb.start_segment].text;
                let last = segments[lb.end_segment - 1].text;
                // Safety: both are subslices of `text`; the range between them is valid UTF-8.
                let start = first.as_ptr() as usize - text.as_ptr() as usize;
                let end = last.as_ptr() as usize - text.as_ptr() as usize + last.len();
                &text[start..end]
            };

            // Per-line direction: re-detect if the paragraph has mixed runs.
            let line_dir = if params.direction.is_none() && lb.start_segment < lb.end_segment {
                slice.paragraph_direction()
            } else {
                dir
            };

            lines.push(LayoutLine {
                text: slice,
                width: lb.width,
                direction: line_dir,
                index: idx,
            });
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(text: &str, max_width: usize) -> Vec<String> {
        Layouter::layout(
            text,
            LayoutParams {
                max_width,
                direction: None,
            },
        )
        .into_iter()
        .map(|l| l.text.to_owned())
        .collect()
    }

    #[test]
    fn single_line_no_wrap() {
        let lines = lay("hello world", 80);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn wraps_at_space() {
        let lines = lay("hello world", 7);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("hello"));
        assert!(lines[1].starts_with("world"));
    }

    #[test]
    fn newline_splits() {
        let lines = lay("foo\nbar", 80);
        assert_eq!(lines, vec!["foo", "bar"]);
    }

    #[test]
    fn cjk_wraps() {
        let lines = lay("中文字", 2);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn direction_auto_ltr() {
        let lines = Layouter::layout("hello", LayoutParams::default());
        assert_eq!(lines[0].direction, Direction::Ltr);
    }

    #[test]
    fn direction_auto_rtl() {
        let lines = Layouter::layout("שלום", LayoutParams::default());
        assert_eq!(lines[0].direction, Direction::Rtl);
    }

    #[test]
    fn direction_override() {
        let lines = Layouter::layout(
            "hello",
            LayoutParams {
                max_width: 0,
                direction: Some(Direction::Rtl),
            },
        );
        assert_eq!(lines[0].direction, Direction::Rtl);
    }

    #[test]
    fn empty_text() {
        let lines = Layouter::layout("", LayoutParams::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "");
    }

    #[test]
    fn width_reported_correctly() {
        let lines = Layouter::layout("hello", LayoutParams::default());
        assert_eq!(lines[0].width, 5);
    }

    #[test]
    fn cjk_width_2_per_char() {
        let lines = Layouter::layout("中", LayoutParams::default());
        assert_eq!(lines[0].width, 2);
    }
}
