//! Simplified Unicode BiDi metadata — port of `bidi.ts` from pretext.
//!
//! We implement the **paragraph-level** BiDi algorithm (Unicode UAX #9 §3.3
//! P1–P3) and per-character strong-type detection.  We do **not** implement
//! the full Level Run / Isolate / Bracket pairing algorithm — that is not
//! needed for terminal rendering where the PTY outputs pre-shaped bytes.
//! This module is used by `analysis` to annotate segments with directionality
//! and by `layout` to set base paragraph direction.

/// Unicode BiDi character type — simplified to the categories we act on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BidiClass {
    /// Strong left-to-right (L).
    L,
    /// Strong right-to-left (R, AL).
    R,
    /// Weak / neutral — directionality determined by context.
    Neutral,
}

/// Resolved paragraph direction.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// Left-to-right (the default).
    #[default]
    Ltr,
    /// Right-to-left (Hebrew, Arabic, …).
    Rtl,
}

/// Determine the BiDi class of a Unicode scalar (P2/P3 strong-type test).
#[must_use]
pub const fn bidi_class(c: char) -> BidiClass {
    let cp = c as u32;
    if is_strong_rtl(cp) {
        BidiClass::R
    } else if is_strong_ltr(cp) {
        BidiClass::L
    } else {
        BidiClass::Neutral
    }
}

/// Determine base paragraph direction by scanning for the first strong
/// directional character (UAX #9 P2–P3).  Returns `Ltr` if none is found.
#[must_use]
pub fn paragraph_direction(text: &str) -> Direction {
    for c in text.chars() {
        match bidi_class(c) {
            BidiClass::L => return Direction::Ltr,
            BidiClass::R => return Direction::Rtl,
            BidiClass::Neutral => {}
        }
    }
    Direction::Ltr
}

/// Whether a run of text is purely LTR (no RTL characters at all).
/// Callers can fast-path layout for the common case.
#[must_use]
pub fn is_ltr_only(text: &str) -> bool {
    text.chars().all(|c| bidi_class(c) != BidiClass::R)
}

// ── Strong RTL ranges (Unicode categories R and AL) ───────────────────────────

const fn is_strong_rtl(cp: u32) -> bool {
    matches!(cp,
        // Hebrew block.
        0x05be | 0x05c0 | 0x05c3 | 0x05c6 |
        0x05d0..=0x05ea | 0x05ef..=0x05f4 |
        // Syriac, Thaana, NKo and adjacent RTL scripts.
        // Note: 0x0700–0x070d ⊂ Syriac; 0x074d–0x07a5 ⊃ Arabic Supplement
        // 0x0750–0x077f so the Arabic Supplement is not listed separately.
        0x0700..=0x070d | 0x070f | 0x0710 | 0x0712..=0x072f |
        0x074d..=0x07a5 | 0x07b1 |
        0x07c0..=0x07ea | 0x07f4..=0x07f5 | 0x07fa |
        // Arabic block (category AL — Arabic Letter).
        0x0600..=0x0605 | 0x0608 | 0x060b | 0x060d |
        0x061b..=0x064a | 0x066d..=0x066f |
        0x0671..=0x06d3 | 0x06d5 |
        0x06e5..=0x06e6 | 0x06ee..=0x06ef |
        0x06fa..=0x06ff |
        // Arabic Extended-A.
        0x08a0..=0x08b4 | 0x08b6..=0x08c7 |
        // Samaritan / Mandaic / Syriac Supplement.
        0x0800..=0x0815 | 0x0824 | 0x0828 |
        0x0830..=0x083e | 0x0840..=0x0858 | 0x085e |
        0x0860..=0x086a |
        // Arabic Presentation Forms-A / -B.
        0xfb1d | 0xfb1f..=0xfb28 | 0xfb2a..=0xfb36 |
        0xfb38..=0xfb3c | 0xfb3e | 0xfb40..=0xfb41 |
        0xfb43..=0xfb44 | 0xfb46..=0xfbc1 |
        0xfbd3..=0xfd3d | 0xfd50..=0xfd8f |
        0xfd92..=0xfdc7 | 0xfdf0..=0xfdfd |
        0xfe70..=0xfe74 | 0xfe76..=0xfefc |
        // Ancient RTL scripts (Old South Arabian, Imperial Aramaic, …).
        0x10800..=0x10806 | 0x10808 | 0x1080a..=0x10835 |
        0x10837..=0x10838 | 0x1083c | 0x1083f..=0x10855 |
        0x10858..=0x1089e | 0x108a7..=0x108af |
        0x108d0..=0x108f2 | 0x108f4..=0x108f5 |
        0x10900..=0x1091b | 0x10920..=0x10939 |
        0x10980..=0x109b7 | 0x109bc..=0x109cf |
        0x109d2..=0x10a00
    )
}

// ── Strong LTR ranges ─────────────────────────────────────────────────────────

const fn is_strong_ltr(cp: u32) -> bool {
    matches!(cp,
        // Basic Latin letters only (digits/punctuation are weak/neutral).
        0x0041..=0x005a | 0x0061..=0x007a |
        // Latin-1 Supplement letters.
        0x00c0..=0x00d6 | 0x00d8..=0x00f6 | 0x00f8..=0x00ff |
        // Latin Extended A/B.
        0x0100..=0x024f |
        // Greek.
        0x0370..=0x03ff |
        // Cyrillic.
        0x0400..=0x04ff |
        // Armenian.
        0x0530..=0x058f |
        // Georgian.
        0x10a0..=0x10ff |
        // Hiragana / Katakana (treated as LTR for paragraph detection).
        0x3040..=0x30ff |
        // CJK Unified Ideographs (LTR for our purposes).
        0x4e00..=0x9fff | 0x3400..=0x4dbf |
        // Hangul Syllables.
        0xac00..=0xd7af
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letter_is_ltr() {
        assert_eq!(bidi_class('A'), BidiClass::L);
        assert_eq!(bidi_class('z'), BidiClass::L);
    }

    #[test]
    fn hebrew_is_rtl() {
        assert_eq!(bidi_class('א'), BidiClass::R);
        assert_eq!(bidi_class('ש'), BidiClass::R);
    }

    #[test]
    fn arabic_is_rtl() {
        assert_eq!(bidi_class('ع'), BidiClass::R);
    }

    #[test]
    fn digit_is_neutral() {
        assert_eq!(bidi_class('5'), BidiClass::Neutral);
    }

    #[test]
    fn paragraph_direction_ltr() {
        assert_eq!(paragraph_direction("hello world"), Direction::Ltr);
    }

    #[test]
    fn paragraph_direction_rtl() {
        assert_eq!(paragraph_direction("שלום"), Direction::Rtl);
    }

    #[test]
    fn paragraph_direction_ltr_first_wins() {
        assert_eq!(paragraph_direction("a שלום"), Direction::Ltr);
    }

    #[test]
    fn paragraph_direction_empty_is_ltr() {
        assert_eq!(paragraph_direction(""), Direction::Ltr);
    }

    #[test]
    fn ltr_only_pure() {
        assert!(is_ltr_only("hello 123"));
    }

    #[test]
    fn ltr_only_rejects_rtl() {
        assert!(!is_ltr_only("hello שלום"));
    }
}
