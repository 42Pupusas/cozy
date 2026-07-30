//! Small measured-text helpers shared by every card and the process table.
//!
//! These have no state of their own -- they're pure functions of a
//! `TextRenderer`'s measurements -- but per this workspace's rule against
//! free functions, they get a zero-sized owner struct instead of living as
//! bare `fn`s.

use statusbar::TextRenderer;

/// Namespace for text-layout helpers that only need a [`TextRenderer`]'s
/// glyph measurements, not any dashboard state.
pub struct TextLayout;

impl TextLayout {
    /// Retain the beginning of a string and replace its overflowing end with
    /// an ellipsis. CPU metadata should read naturally from the start (for
    /// example, `Intel(R) Core(TM) … · 16 cores`) rather than exposing an
    /// arbitrary suffix of the brand, and the same logic serves the process
    /// table's long command names.
    #[must_use]
    pub fn truncate_prefix(text: &str, renderer: &mut TextRenderer, max_width: i32) -> String {
        if max_width <= 0 {
            return String::new();
        }
        if renderer.measure(text) <= max_width {
            return text.to_owned();
        }
        let ellipsis_width = renderer.measure("…");
        if ellipsis_width >= max_width {
            return "…".to_owned();
        }
        let mut prefix = String::new();
        for character in text.chars() {
            let mut candidate = prefix.clone();
            candidate.push(character);
            candidate.push('…');
            if renderer.measure(&candidate) > max_width {
                break;
            }
            prefix.push(character);
        }
        format!("{prefix}…")
    }
}

#[cfg(test)]
mod tests {
    use super::TextLayout;
    use statusbar::TextRenderer;

    #[test]
    fn short_text_is_returned_unchanged() {
        let mut renderer = TextRenderer::load(None, 13.0);
        let text = "cpu";
        assert_eq!(
            TextLayout::truncate_prefix(text, &mut renderer, 1000),
            "cpu"
        );
    }

    #[test]
    fn long_text_is_truncated_with_a_trailing_ellipsis() {
        let mut renderer = TextRenderer::load(None, 13.0);
        let long = "a".repeat(200);
        let narrow_width = renderer.measure("aaaa");
        let truncated = TextLayout::truncate_prefix(&long, &mut renderer, narrow_width);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() < long.len());
    }

    #[test]
    fn zero_or_negative_width_yields_an_empty_string() {
        let mut renderer = TextRenderer::load(None, 13.0);
        assert_eq!(
            TextLayout::truncate_prefix("anything", &mut renderer, 0),
            ""
        );
        assert_eq!(
            TextLayout::truncate_prefix("anything", &mut renderer, -5),
            ""
        );
    }
}
