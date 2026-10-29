//! Grapheme clusters: the codepoints that join onto one cell, and the
//! transport form a row's clusters take to the renderer.

use text_layout::measurement::CellWidth as _;

/// Zero width joiner: the codepoint after it joins the same cluster.
pub const ZWJ: char = '\u{200d}';

/// Unicode properties that decide whether a codepoint with width still joins
/// the previous cell, and which zero-width codepoints are drawn.
pub trait ClusterChar {
    /// U+1F1E6..U+1F1FF. Two in a row form one flag.
    fn is_regional_indicator(&self) -> bool;
    /// Skin tone modifiers U+1F3FB..U+1F3FF, which join the emoji before them.
    fn is_emoji_modifier(&self) -> bool;
    /// A zero-width codepoint with its own glyph to draw over the base: a
    /// combining mark, not a joiner, variation selector or format character.
    fn is_overlay_mark(&self) -> bool;
}

impl ClusterChar for char {
    #[inline]
    fn is_regional_indicator(&self) -> bool {
        matches!(self, '\u{1f1e6}'..='\u{1f1ff}')
    }

    #[inline]
    fn is_emoji_modifier(&self) -> bool {
        matches!(self, '\u{1f3fb}'..='\u{1f3ff}')
    }

    fn is_overlay_mark(&self) -> bool {
        self.cell_width() == 0
            && !matches!(
                self,
                '\u{00ad}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{fe00}'..='\u{fe0f}'
                    | '\u{feff}'
                    | '\u{e0000}'..='\u{e01ef}'
            )
    }
}

/// The cluster in one cell of a transported row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// Column of the cell, which holds the base codepoint.
    pub col: u16,
    /// Every codepoint of the cluster, base first.
    pub text: String,
}

impl Cluster {
    #[must_use]
    pub fn new(col: u16, codepoints: &[char]) -> Self {
        Self {
            col,
            text: codepoints.iter().collect(),
        }
    }

    /// Codepoints to draw over the base glyph. Without text shaping an emoji
    /// sequence cannot be drawn as one glyph, so everything from the first
    /// joiner on is left to the base glyph alone.
    pub fn marks(&self) -> impl Iterator<Item = char> + '_ {
        self.text
            .chars()
            .skip(1)
            .take_while(|&ch| ch != ZWJ)
            .filter(ClusterChar::is_overlay_mark)
    }
}

#[cfg(test)]
mod tests {
    use super::{Cluster, ClusterChar};

    #[test]
    fn marks_are_combining_marks_before_any_joiner() {
        let accent = Cluster::new(0, &['e', '\u{301}', '\u{fe0f}', '\u{323}']);
        assert_eq!(accent.marks().collect::<String>(), "\u{301}\u{323}");
        let family = Cluster::new(0, &['\u{1f468}', '\u{200d}', '\u{1f469}']);
        assert_eq!(family.marks().count(), 0);
    }

    #[test]
    fn joining_properties() {
        assert!('\u{1f1fa}'.is_regional_indicator());
        assert!(!'A'.is_regional_indicator());
        assert!('\u{1f3fd}'.is_emoji_modifier());
        assert!('\u{301}'.is_overlay_mark());
        assert!(!'\u{200d}'.is_overlay_mark());
    }
}
