//! dmenu mode: rank arbitrary caller-supplied lines instead of `.desktop`
//! apps.
//!
//! This is the pure half of `launcher --dmenu`, the drop-in replacement for
//! `fuzzel --dmenu` / `rofi -dmenu`: read newline-separated choices on
//! stdin, let the user pick one, print it back on stdout. Every consumer in
//! this setup is a shell pipeline (`clipboard list | launcher --dmenu`,
//! `power-menu.sh`), so the contract is deliberately identical to the tools
//! it replaces -- swapping the binary name in a pipe must be the *only*
//! edit needed.
//!
//! Kept free of Wayland/rendering knowledge (like [`crate::Catalog`], its
//! `.desktop` sibling) so the selection/ranking rules are unit-testable
//! headlessly; `bins/launcher` supplies the UI.

use crate::fuzzy::FuzzyMatch;

/// One input line's fuzzy-match result, paired with the line itself --
/// dmenu's analogue of [`crate::RankedEntry`].
pub struct RankedChoice<'a> {
    pub choice: &'a str,
    pub fuzzy: FuzzyMatch,
}

/// A set of caller-supplied choices, searchable exactly like the app
/// catalog.
///
/// Ownership rather than borrowing: the lines come from stdin, which is
/// drained and closed before the UI opens (a pipe's writer typically exits
/// immediately -- `clipboard list` does), so there's no upstream buffer to
/// borrow from.
pub struct ChoiceList {
    choices: Vec<String>,
}

impl ChoiceList {
    /// Parse newline-separated `input` into choices.
    ///
    /// Blank lines are dropped rather than kept as empty rows: a trailing
    /// newline on the final line is near-universal in shell output (every
    /// tool in these pipelines emits one), and honouring it literally would
    /// put a blank, un-selectable row at the bottom of every menu.
    /// Carriage returns are trimmed too, so a stray CRLF source doesn't
    /// leave an invisible `\r` on the value we print back.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        Self {
            choices: input
                .lines()
                .map(|line| line.trim_end_matches('\r'))
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Rank every choice against `query`, best first.
    ///
    /// Unlike [`crate::Catalog::search`], ties break by *input order*, not
    /// alphabetically: a dmenu caller's line order is meaningful (the
    /// clipboard history is newest-first, a power menu is authored in a
    /// deliberate order), so re-sorting it alphabetically would scramble
    /// the caller's intent. `sort_by` is stable, so equal scores keep the
    /// order they were piped in.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<RankedChoice<'_>> {
        let mut ranked: Vec<RankedChoice<'_>> = self
            .choices
            .iter()
            .filter_map(|choice| {
                let fuzzy = FuzzyMatch::find(query, choice)?;
                Some(RankedChoice {
                    choice: choice.as_str(),
                    fuzzy,
                })
            })
            .collect();

        ranked.sort_by_key(|ranked| std::cmp::Reverse(ranked.fuzzy.score));
        ranked
    }

    /// Number of choices parsed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.choices.len()
    }

    /// Whether no choices were parsed -- the caller piped in nothing, and
    /// the UI should say so rather than open an empty window the user can
    /// only escape from.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::ChoiceList;

    #[test]
    fn parses_newline_separated_lines() {
        let list = ChoiceList::parse("alpha\nbeta\ngamma");
        assert_eq!(list.len(), 3);
    }

    /// Shell output essentially always ends in a newline; keeping it as an
    /// empty choice would put a blank row at the bottom of every menu.
    #[test]
    fn trailing_newline_does_not_create_empty_choice() {
        let list = ChoiceList::parse("alpha\nbeta\n");
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn blank_and_crlf_lines_are_cleaned() {
        let list = ChoiceList::parse("alpha\r\n\n\nbeta\r\n");
        let all = list.search("");
        assert_eq!(
            all.iter().map(|r| r.choice).collect::<Vec<_>>(),
            ["alpha", "beta"],
            "CR must be trimmed and blank lines dropped"
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(ChoiceList::parse("").is_empty());
        assert!(ChoiceList::parse("\n\n").is_empty());
    }

    /// The empty query is the state the menu opens in -- it must show
    /// everything, in the caller's original order.
    #[test]
    fn empty_query_preserves_input_order() {
        let list = ChoiceList::parse("zebra\napple\nmango");
        let ranked = list.search("");
        assert_eq!(
            ranked.iter().map(|r| r.choice).collect::<Vec<_>>(),
            ["zebra", "apple", "mango"],
            "input order is meaningful (clipboard history is newest-first)"
        );
    }

    #[test]
    fn query_filters_and_ranks() {
        let list = ChoiceList::parse("firefox\nthunderbird\nfiles");
        let ranked = list.search("fi");
        assert_eq!(
            ranked.iter().map(|r| r.choice).collect::<Vec<_>>(),
            ["firefox", "files"],
            "non-matching lines are dropped"
        );
    }

    /// Clipboard history lines are `id<TAB>preview`; the id must remain
    /// matchable and the whole line must survive intact for `cut -f1`.
    #[test]
    fn tab_separated_lines_survive_intact() {
        let list = ChoiceList::parse("0\thello world\n1\tgoodbye");
        let ranked = list.search("goodbye");
        assert_eq!(ranked.len(), 1);
        assert_eq!(
            ranked[0].choice, "1\tgoodbye",
            "the tab and id must be preserved for the downstream `cut -f1`"
        );
    }

    #[test]
    fn no_match_yields_nothing() {
        let list = ChoiceList::parse("alpha\nbeta");
        assert!(list.search("zzzz").is_empty());
    }
}
