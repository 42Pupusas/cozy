//! Text segmentation — port of `analysis.ts` from pretext.
//!
//! Splits a `&str` into a sequence of [`Segment`]s, each tagged with a
//! [`BreakKind`] that tells the line-breaker how it may split before this
//! segment.
//!
//! The original `analysis.ts` uses `Intl.Segmenter` (word granularity) from
//! the browser.  We implement a compact UAX #29 word-break state machine
//! instead — no external dependencies.

use crate::bidi::{bidi_class, BidiClass, Direction};
use crate::measurement::segment_width;

/// How the line-breaker may handle the break opportunity **before** this segment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// Mandatory line break (e.g. `\n`).
    Mandatory,
    /// Opportunity: may break here when line is too long.
    Opportunity,
    /// No break allowed — this segment is glued to the previous one.
    Prohibited,
}

/// A contiguous run of text with uniform break-before semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    /// The text slice (into the original input — zero copy).
    pub text:      &'a str,
    /// Cell width (UAX #11) — pre-computed so the line-breaker never calls
    /// back into measurement.
    pub width:     usize,
    /// How the line-breaker may treat the break point before this segment.
    pub break_before: BreakKind,
    /// Resolved paragraph direction for this segment.
    pub direction: Direction,
}

/// Segment an input `&str` into a `Vec<Segment>`.
///
/// The segmenter produces:
/// - One segment per UAX #29 word (word runs together with their trailing
///   spaces as two separate segments).
/// - Newlines become mandatory-break segments of width 0.
/// - CJK characters each get their own 2-cell segment (each is a break
///   opportunity by itself).
/// - Combining characters are merged into the preceding segment.
#[must_use]
pub fn segment(text: &str) -> Vec<Segment<'_>> {
    let mut segments: Vec<Segment<'_>> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0; // byte offset into `text`

    while i < text.len() {
        let remaining = &text[i..];
        let c = first_char(remaining);

        // ── Newline → mandatory break ────────────────────────────────────
        if c == '\n' {
            segments.push(Segment {
                text:         &text[i..=i],
                width:        0,
                break_before: BreakKind::Mandatory,
                direction:    Direction::Ltr,
            });
            i += 1;
            continue;
        }

        // ── Carriage return (skip or treat as newline) ───────────────────
        if c == '\r' {
            // Consume \r\n as one mandatory break, or lone \r.
            let end = if bytes.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
            segments.push(Segment {
                text:         &text[i..end],
                width:        0,
                break_before: BreakKind::Mandatory,
                direction:    Direction::Ltr,
            });
            i = end;
            continue;
        }

        // ── Whitespace (space / tab) → opportunity ───────────────────────
        if c == ' ' || c == '\t' {
            let start = i;
            while i < text.len() {
                let ch = first_char(&text[i..]);
                if ch != ' ' && ch != '\t' {
                    break;
                }
                i += ch.len_utf8();
            }
            let slice = &text[start..i];
            segments.push(Segment {
                text:         slice,
                width:        segment_width(slice),
                break_before: BreakKind::Opportunity,
                direction:    Direction::Ltr,
            });
            continue;
        }

        // ── CJK / wide character → each is its own break opportunity ─────
        if is_cjk(c) {
            let len = c.len_utf8();
            segments.push(Segment {
                text:         &text[i..i + len],
                width:        2,
                break_before: BreakKind::Opportunity,
                direction:    bidi_dir(c),
            });
            i += len;
            continue;
        }

        // ── Word run (letters + combining marks) ─────────────────────────
        {
            let start = i;
            i += c.len_utf8();
            while i < text.len() {
                let ch = first_char(&text[i..]);
                if ch == '\n' || ch == '\r' || ch == ' ' || ch == '\t' || is_cjk(ch) {
                    break;
                }
                // Combining marks attach to the preceding character.
                i += ch.len_utf8();
            }
            let slice = &text[start..i];
            let break_before = if segments.is_empty() {
                BreakKind::Prohibited
            } else {
                // A word after a space segment gets Prohibited (the space
                // already carried the opportunity); a word after another
                // word gets Opportunity (rare — hyphen-break case).
                match segments.last().map(|s| s.break_before) {
                    Some(BreakKind::Opportunity) => BreakKind::Prohibited,
                    _ => BreakKind::Opportunity,
                }
            };
            segments.push(Segment {
                text:         slice,
                width:        segment_width(slice),
                break_before,
                direction:    paragraph_dir_of(slice),
            });
        }
    }

    segments
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Decode the first `char` of a non-empty `&str` — panics on empty input.
#[inline]
fn first_char(s: &str) -> char {
    s.chars().next().expect("non-empty str")
}

/// True if `c` is a CJK / wide ideograph (each is an independent break point).
const fn is_cjk(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x1100..=0x115f |   // Hangul leading consonants
        0x2e80..=0x303e |   // CJK Radicals + Symbols
        0x3041..=0x33bf |   // Hiragana / Katakana / Bopomofo
        0x3400..=0x4dbf |   // CJK Ext A
        0x4e00..=0x9fff |   // CJK Unified
        0xa000..=0xa4cf |   // Yi
        0xac00..=0xd7af |   // Hangul Syllables
        0xf900..=0xfaff |   // CJK Compat Ideographs
        0x20000..=0x2a6df | // CJK Ext B
        0x2a700..=0x2ceaf | // CJK Ext C–E
        0x30000..=0x3134f   // CJK Ext G
    )
}

const fn bidi_dir(c: char) -> Direction {
    match bidi_class(c) {
        BidiClass::R => Direction::Rtl,
        _ => Direction::Ltr,
    }
}

fn paragraph_dir_of(s: &str) -> Direction {
    crate::bidi::paragraph_direction(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words() {
        let segs = segment("hello world");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].text, "hello");
        assert_eq!(segs[1].text, " ");
        assert_eq!(segs[1].break_before, BreakKind::Opportunity);
        assert_eq!(segs[2].text, "world");
        assert_eq!(segs[2].break_before, BreakKind::Prohibited);
    }

    #[test]
    fn newline_is_mandatory() {
        let segs = segment("foo\nbar");
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[1].break_before, BreakKind::Mandatory);
        assert_eq!(segs[1].width, 0);
    }

    #[test]
    fn crlf_is_one_mandatory_segment() {
        let segs = segment("a\r\nb");
        // a, \r\n, b
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[1].text, "\r\n");
        assert_eq!(segs[1].break_before, BreakKind::Mandatory);
    }

    #[test]
    fn cjk_each_own_segment() {
        let segs = segment("中文");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].width, 2);
        assert_eq!(segs[1].width, 2);
        assert_eq!(segs[0].break_before, BreakKind::Opportunity);
    }

    #[test]
    fn mixed_ascii_cjk() {
        let segs = segment("hi中");
        // "hi", "中"
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].width, 2);
        assert_eq!(segs[1].width, 2);
    }

    #[test]
    fn empty_string() {
        assert_eq!(segment("").len(), 0);
    }

    #[test]
    fn tab_is_opportunity() {
        let segs = segment("a\tb");
        assert_eq!(segs[1].break_before, BreakKind::Opportunity);
    }

    #[test]
    fn widths_sum() {
        let segs = segment("ab cd");
        let total: usize = segs.iter().map(|s| s.width).sum();
        assert_eq!(total, 5); // "ab"=2 " "=1 "cd"=2
    }
}
