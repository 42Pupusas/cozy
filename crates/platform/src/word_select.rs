//! Word-boundary detection over a rendered row of cells.
//!
//! A double-click should select the "word" under the pointer, and what counts
//! as a word in a *terminal* is not what counts as a word in prose: the text
//! under the cursor is usually a path, a flag, a URL, or an identifier. So the
//! rule here is the one xterm/kitty converge on -- expand outward from the
//! clicked column while the neighbouring cell belongs to the same *character
//! class* -- with the word class deliberately generous (it swallows `_ - . /`
//! and friends) so `src/main.rs` or `--release` come out whole instead of
//! shredded at every punctuation mark.
//!
//! Kept in its own module (and free of any pointer/window state) for the same
//! reason [`crate::link_scan`] is: "given this row of cells and a column,
//! which columns form the word" is a pure question with one answer, and the
//! decision about what a double-click *means* belongs to the window.

use grid::Cell;

/// Which class a cell's character belongs to for word-expansion purposes.
///
/// Expansion continues while the neighbour shares the clicked cell's class,
/// which is what makes `foo(bar)` split into `foo`, `(`, `bar`, `)` rather
/// than selecting the whole run: symbols are their own class, so a click on
/// `(` selects just the bracket run, not the identifiers around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    /// Blank space. Never selected -- see [`WordBoundary::at`].
    Space,
    /// Identifier-ish text, including the punctuation that shows up *inside*
    /// terminal words (paths, flags, hostnames, versions).
    Word,
    /// Anything else: brackets, quotes, operators. Selected as its own run.
    Symbol,
}

impl CharClass {
    /// Classify one character.
    ///
    /// The extra word characters are the ones that appear *within* tokens a
    /// user double-clicks to copy: `/` and `.` for paths, `-` and `_` for
    /// flags and identifiers, `~` for home-relative paths, `:` and `@` for
    /// `host:port` / `user@host`, `+` and `=` for version strings and
    /// `KEY=value`. `,` and `;` are excluded on purpose -- they are almost
    /// always list separators the user does *not* want in the copy.
    fn of(ch: char) -> Self {
        if ch == ' ' || ch == '\t' || ch == '\0' {
            Self::Space
        } else if ch.is_alphanumeric()
            || matches!(ch, '_' | '-' | '.' | '/' | '~' | ':' | '@' | '+' | '=')
        {
            Self::Word
        } else {
            Self::Symbol
        }
    }
}

/// Stateless word-boundary finder over a row of rendered cells.
///
/// A struct rather than loose logic so the policy ("what is a word here")
/// has a name to hang the doc comment and any future configurability off of.
pub struct WordBoundary;

impl WordBoundary {
    /// The inclusive `(start, end)` index span of the word covering `idx`,
    /// or `None` when there is nothing worth selecting there (out of range,
    /// or blank space -- double-clicking the empty right-hand padding of a
    /// row should select nothing, not a screen-wide run of spaces).
    #[must_use]
    pub fn at(line: &[Cell], idx: usize) -> Option<(usize, usize)> {
        if idx >= line.len() {
            return None;
        }
        let classes = Self::classify(line);
        let target = classes[idx];
        if target == CharClass::Space {
            return None;
        }

        let mut start = idx;
        while start > 0 && classes[start - 1] == target {
            start -= 1;
        }
        let mut end = idx;
        while end + 1 < classes.len() && classes[end + 1] == target {
            end += 1;
        }
        Some((start, end))
    }

    /// Per-column classes for a row, with the trailing halves of wide glyphs
    /// inheriting the class of their lead cell.
    ///
    /// A wide glyph occupies two columns: the lead carries the character and
    /// the spacer carries a placeholder space. Classifying that placeholder
    /// literally would insert a phantom `Space` in the middle of a CJK word
    /// and cut every such selection short after one glyph.
    fn classify(line: &[Cell]) -> Vec<CharClass> {
        let mut classes = Vec::with_capacity(line.len());
        for cell in line {
            let class = if cell.spacer() {
                classes.last().copied().unwrap_or(CharClass::Space)
            } else {
                CharClass::of(cell.ch)
            };
            classes.push(class);
        }
        classes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row of plain cells from a string.
    fn row(s: &str) -> Vec<Cell> {
        s.chars()
            .map(|ch| Cell::new(ch, grid::Attrs::default(), 0))
            .collect()
    }

    /// The text of the span `at` reports, for readable assertions.
    fn word_at(s: &str, col: usize) -> Option<String> {
        let cells = row(s);
        let (a, b) = WordBoundary::at(&cells, col)?;
        Some(s.chars().skip(a).take(b - a + 1).collect())
    }

    #[test]
    fn expands_to_the_whole_word_from_any_column_in_it() {
        for col in 6..=10 {
            assert_eq!(word_at("hello world!", col).as_deref(), Some("world"));
        }
    }

    #[test]
    fn stops_at_word_start_and_end_of_line() {
        assert_eq!(word_at("hello world", 0).as_deref(), Some("hello"));
        assert_eq!(word_at("hello world", 10).as_deref(), Some("world"));
    }

    #[test]
    fn paths_and_flags_stay_whole() {
        assert_eq!(
            word_at("run src/main.rs now", 8).as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(word_at("cargo --release", 8).as_deref(), Some("--release"));
        assert_eq!(
            word_at("cd ~/Code/forfun", 5).as_deref(),
            Some("~/Code/forfun")
        );
        assert_eq!(
            word_at("at http://127.0.0.1:8080/x", 10).as_deref(),
            Some("http://127.0.0.1:8080/x")
        );
    }

    #[test]
    fn symbols_form_their_own_run() {
        // Clicking the bracket selects the bracket, not the identifier.
        assert_eq!(word_at("foo(bar)", 3).as_deref(), Some("("));
        assert_eq!(word_at("foo(bar)", 1).as_deref(), Some("foo"));
        assert_eq!(word_at("foo(bar)", 5).as_deref(), Some("bar"));
    }

    #[test]
    fn blank_space_selects_nothing() {
        assert_eq!(word_at("hello world", 5), None);
        assert_eq!(word_at("   ", 1), None);
    }

    #[test]
    fn out_of_range_column_selects_nothing() {
        assert_eq!(word_at("hi", 9), None);
        assert_eq!(WordBoundary::at(&[], 0), None);
    }

    #[test]
    fn wide_glyph_spacers_do_not_split_a_word() {
        // "中文" occupies four columns: lead, spacer, lead, spacer.
        let attrs = grid::Attrs::default();
        let cells = vec![
            Cell::wide_lead('中', attrs, 0),
            Cell::wide_spacer(attrs, 0),
            Cell::wide_lead('文', attrs, 0),
            Cell::wide_spacer(attrs, 0),
            Cell::new(' ', attrs, 0),
        ];
        assert_eq!(WordBoundary::at(&cells, 0), Some((0, 3)));
        assert_eq!(WordBoundary::at(&cells, 2), Some((0, 3)));
    }
}
