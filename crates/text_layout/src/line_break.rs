//! Line-breaking — port of `line-break.ts` from pretext.
//!
//! Given a slice of pre-segmented, pre-measured [`Segment`]s and a maximum
//! line width in cells, produces a `Vec<LineBreak>` describing where to split.
//!
//! This is a **pure arithmetic greedy walk** — no look-ahead, no dynamic
//! programming.  The algorithm is identical to the TypeScript original; the
//! only differences are types and no heap for intermediate state.

use crate::analysis::{BreakKind, Segment};

/// A line-break decision.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LineBreak {
    /// Index into `segments` where this line begins.
    pub start_segment: usize,
    /// Index (exclusive) of the last segment on this line.
    pub end_segment:   usize,
    /// Total cell width of the line (trailing whitespace excluded).
    pub width:         usize,
}

// Recorded break opportunity: "this line can end here".
//
// `line_end`   — exclusive segment index for the finished line.
// `next_start` — segment index for the next line (skip whitespace).
// `width`      — visual width of the line up to (not including) trailing space.
#[derive(Copy, Clone)]
struct Opportunity {
    line_end:   usize,
    next_start: usize,
    width:      usize,
}

/// Compute line-break positions for a sequence of segments.
///
/// # Parameters
/// - `segments` — output of [`crate::analysis::segment`].
/// - `max_width` — maximum line width in cells (`0` means unlimited).
///
/// # Returns
/// One `LineBreak` per line, in order.  An empty input produces one empty line.
#[must_use]
pub fn compute_breaks<'a>(segments: &'a [Segment<'a>], max_width: usize) -> Vec<LineBreak> {
    if segments.is_empty() {
        return vec![LineBreak { start_segment: 0, end_segment: 0, width: 0 }];
    }

    let unlimited            = max_width == 0;
    let mut lines:            Vec<LineBreak>      = Vec::new();
    let mut line_start:       usize               = 0;
    let mut line_width:       usize               = 0;
    let mut last_opportunity: Option<Opportunity> = None;
    let mut i:                usize               = 0;

    while i < segments.len() {
        let seg = &segments[i];

        match seg.break_before {
            // ── Mandatory newline ─────────────────────────────────────────
            BreakKind::Mandatory => {
                lines.push(LineBreak {
                    start_segment: line_start,
                    end_segment:   i,
                    width:         line_width,
                });
                line_start       = i + 1;
                line_width       = 0;
                last_opportunity = None;
                i               += 1;
            }

            // ── Break opportunity ─────────────────────────────────────────
            BreakKind::Opportunity => {
                let ws = is_whitespace_seg(seg.text);

                if !unlimited && line_width > 0 && line_width + seg.width > max_width {
                    // Overflow at an opportunity.
                    if ws {
                        // The space itself overflows → break immediately before it,
                        // skip the space on the new line.
                        lines.push(LineBreak {
                            start_segment: line_start,
                            end_segment:   i,
                            width:         line_width,
                        });
                        line_start       = i + 1;
                        line_width       = 0;
                        last_opportunity = None;
                        i               += 1;
                    } else {
                        // Non-whitespace opportunity (CJK) that overflows →
                        // roll back to the last opportunity or force-break here.
                        let (end, next, w) = rollback_or_here(last_opportunity.as_ref(), i, line_width);
                        lines.push(LineBreak { start_segment: line_start, end_segment: end, width: w });
                        line_start       = next;
                        line_width       = 0;
                        last_opportunity = None;
                        i                = next;
                    }
                    continue;
                }

                // Fits — add segment and record opportunity.
                if ws {
                    // Whitespace: opportunity is BEFORE this space (line ends
                    // before it; next line starts after it).
                    last_opportunity = Some(Opportunity {
                        line_end:   i,
                        next_start: i + 1,
                        width:      line_width,
                    });
                    line_width += seg.width;
                } else {
                    // CJK: opportunity is AFTER this char (it belongs to
                    // the current line; next line starts at i+1).
                    line_width += seg.width;
                    last_opportunity = Some(Opportunity {
                        line_end:   i + 1,
                        next_start: i + 1,
                        width:      line_width,
                    });
                }
                i += 1;
            }

            // ── No break allowed ──────────────────────────────────────────
            BreakKind::Prohibited => {
                if !unlimited && line_width > 0 && line_width + seg.width > max_width {
                    // Roll back to the last opportunity, or force-break before here.
                    let (end, next, w) = rollback_or_here(last_opportunity.as_ref(), i, line_width);
                    lines.push(LineBreak { start_segment: line_start, end_segment: end, width: w });
                    line_start       = next;
                    line_width       = 0;
                    last_opportunity = None;
                    i                = next;
                    continue;
                }
                line_width += seg.width;
                i          += 1;
            }
        }
    }

    // Flush the final line.
    lines.push(LineBreak {
        start_segment: line_start,
        end_segment:   segments.len(),
        width:         line_width,
    });

    lines
}

/// Roll back to `last_opportunity` if set, or force-break immediately before `i`.
#[inline]
const fn rollback_or_here(
    last_opportunity: Option<&Opportunity>,
    i:                usize,
    line_width:       usize,
) -> (usize, usize, usize) {
    match last_opportunity {
        Some(op) => (op.line_end, op.next_start, op.width),
        None     => (i, i, line_width),
    }
}

/// True if every byte in `s` is an ASCII space or tab.
#[inline]
fn is_whitespace_seg(s: &str) -> bool {
    s.bytes().all(|b| b == b' ' || b == b'\t')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::segment;

    fn break_text(text: &str, max_width: usize) -> Vec<String> {
        let segs = segment(text);
        let breaks = compute_breaks(&segs, max_width);
        breaks.iter().map(|lb| {
            segs[lb.start_segment..lb.end_segment]
                .iter()
                .map(|s| s.text)
                .collect::<String>()
        }).collect()
    }

    #[test]
    fn no_break_needed() {
        let lines = break_text("hello world", 80);
        assert_eq!(lines, vec!["hello world"]);
    }

    #[test]
    fn break_at_space() {
        let lines = break_text("hello world", 7);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], "world");
    }

    #[test]
    fn multi_word_wrapping() {
        // "aa bb cc" at width 5: "aa bb" (5 cells) | "cc"
        let lines = break_text("aa bb cc", 5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "aa bb");
        assert_eq!(lines[1], "cc");
    }

    #[test]
    fn mandatory_newline() {
        let lines = break_text("foo\nbar", 80);
        assert_eq!(lines, vec!["foo", "bar"]);
    }

    #[test]
    fn mandatory_crlf() {
        let lines = break_text("foo\r\nbar", 80);
        assert_eq!(lines, vec!["foo", "bar"]);
    }

    #[test]
    fn unlimited_width() {
        let lines = break_text("a b c d e f g", 0);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn cjk_breaks_per_character() {
        let lines = break_text("中文字", 2);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "中");
        assert_eq!(lines[1], "文");
        assert_eq!(lines[2], "字");
    }

    #[test]
    fn empty_string_one_empty_line() {
        let segs = segment("");
        let breaks = compute_breaks(&segs, 80);
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0].width, 0);
    }

    #[test]
    fn long_word_forced_break() {
        // A single word longer than max_width — must appear on one line (no infinite loop).
        let lines = break_text("superlongword", 5);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "superlongword");
    }

    #[test]
    fn three_words_tight() {
        // "ab cd ef" at width 5: "ab cd" (5) | "ef"
        let lines = break_text("ab cd ef", 5);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "ab cd");
        assert_eq!(lines[1], "ef");
    }
}
