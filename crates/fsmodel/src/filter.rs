//! Which entries of a directory are currently shown.
//!
//! Separate from sorting because the two change independently and at
//! different rates: the hidden-files toggle fires once in a while, whereas
//! a type-to-filter query changes on every keystroke. Keeping them apart
//! means a keystroke re-filters without re-sorting the whole listing.
//!
//! Deliberately **not** fuzzy: `launcher::fuzzy` owns fuzzy scoring, and
//! duplicating it here would be exactly the kind of workspace non-DRYness
//! we're trying to avoid. This crate answers the objective question ("does
//! this entry pass?"); ranked fuzzy search is layered on top by the UI.

use crate::entry::Entry;

/// The visibility rules applied to a directory's entries.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Show entries whose names begin with `.`.
    ///
    /// Off by default, matching every mainstream file manager and the
    /// long-standing Unix convention.
    pub show_hidden: bool,
    /// A case-insensitive substring the name must contain. Empty means "no
    /// name filtering" -- stored as a plain `String` so the UI can bind it
    /// straight to a text input without an Option dance.
    pub name_contains: String,
}

impl Filter {
    /// Whether `entry` should be visible.
    #[must_use]
    pub fn accepts(&self, entry: &Entry) -> bool {
        if entry.hidden && !self.show_hidden {
            return false;
        }
        if self.name_contains.is_empty() {
            return true;
        }
        // Lowercase both sides for a case-insensitive contains. This
        // allocates per call, which is fine at directory scale but is the
        // first thing to optimise if profiling ever says otherwise.
        entry
            .name
            .to_lowercase()
            .contains(&self.name_contains.to_lowercase())
    }

    /// Whether any rule is actually active.
    ///
    /// Lets a caller skip the filtering pass entirely -- and lets the UI
    /// show a "filtered" indicator only when something is really being
    /// hidden by the query.
    #[must_use]
    pub const fn is_inactive(&self) -> bool {
        self.show_hidden && self.name_contains.is_empty()
    }

    /// Flip the hidden-files toggle (Ctrl+H in most file managers).
    pub const fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn entry(dir: &TempDir, name: &str) -> Entry {
        dir.create_file(name, b"x");
        Entry::from_path(&dir.path().join(name)).expect("entry")
    }

    #[test]
    fn hides_dotfiles_by_default() {
        let dir = TempDir::new("filter_hidden");
        let hidden = entry(&dir, ".bashrc");
        let visible = entry(&dir, "notes.txt");

        let filter = Filter::default();
        assert!(!filter.accepts(&hidden));
        assert!(filter.accepts(&visible));
    }

    #[test]
    fn shows_dotfiles_when_enabled() {
        let dir = TempDir::new("filter_show_hidden");
        let hidden = entry(&dir, ".bashrc");

        let filter = Filter {
            show_hidden: true,
            ..Filter::default()
        };
        assert!(filter.accepts(&hidden));
    }

    #[test]
    fn name_filter_is_case_insensitive() {
        let dir = TempDir::new("filter_name_case");
        let e = entry(&dir, "ReadMe.md");

        let filter = Filter {
            name_contains: "readme".to_owned(),
            ..Filter::default()
        };
        assert!(filter.accepts(&e));
    }

    #[test]
    fn name_filter_rejects_non_matches() {
        let dir = TempDir::new("filter_name_reject");
        let e = entry(&dir, "notes.txt");

        let filter = Filter {
            name_contains: "zzz".to_owned(),
            ..Filter::default()
        };
        assert!(!filter.accepts(&e));
    }

    /// A name query must not resurrect hidden files: the two rules are
    /// AND-ed, so searching for "bash" while hidden files are off should
    /// still not reveal `.bashrc`.
    #[test]
    fn name_filter_does_not_override_hidden_rule() {
        let dir = TempDir::new("filter_combined");
        let hidden = entry(&dir, ".bashrc");

        let filter = Filter {
            show_hidden: false,
            name_contains: "bash".to_owned(),
        };
        assert!(!filter.accepts(&hidden));
    }

    #[test]
    fn toggling_hidden_flips_the_flag() {
        let mut filter = Filter::default();
        assert!(!filter.show_hidden);
        filter.toggle_hidden();
        assert!(filter.show_hidden);
        filter.toggle_hidden();
        assert!(!filter.show_hidden);
    }

    #[test]
    fn inactive_only_when_nothing_is_filtered() {
        assert!(
            !Filter::default().is_inactive(),
            "hiding dotfiles is active"
        );
        let all_visible = Filter {
            show_hidden: true,
            name_contains: String::new(),
        };
        assert!(all_visible.is_inactive());
    }
}
