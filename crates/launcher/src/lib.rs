//! Pure app-launcher logic: `.desktop` discovery, fuzzy search, and
//! `Exec=` spawning -- no Wayland/rendering knowledge, so it's headlessly
//! unit-testable and reusable by any launcher UI.

pub mod app_index;
pub mod desktop_entry;
pub mod fuzzy;
pub mod icon_lookup;
pub mod launch;
pub mod singleton;

use app_index::AppIndex;
use desktop_entry::DesktopEntry;
use fuzzy::FuzzyMatch;

/// One entry's fuzzy-match result against the current query, paired with
/// the entry itself -- what a launcher UI actually renders one row from.
pub struct RankedEntry<'a> {
    pub entry: &'a DesktopEntry,
    pub fuzzy: FuzzyMatch,
}

/// The full app catalog plus the query-ranking step, bundled so a UI just
/// calls [`Catalog::search`] each keystroke instead of re-deriving the
/// scan/filter/sort pipeline itself.
pub struct Catalog {
    index: AppIndex,
}

impl Catalog {
    /// Scan the system's `.desktop` files once. Rescanning (e.g. to notice
    /// newly-installed apps) means constructing a new `Catalog`.
    #[must_use]
    pub fn load() -> Self {
        Self {
            index: AppIndex::scan(),
        }
    }

    /// Rank every entry against `query`, dropping non-matches, and return
    /// them best-match-first (ties broken alphabetically by name so the
    /// order is stable and predictable). An empty query returns every
    /// entry alphabetically, matching how most launchers show the full
    /// list before you start typing.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<RankedEntry<'_>> {
        let mut ranked: Vec<RankedEntry<'_>> = self
            .index
            .entries
            .iter()
            .filter_map(|entry| {
                let fuzzy = FuzzyMatch::find(query, &entry.name)?;
                Some(RankedEntry { entry, fuzzy })
            })
            .collect();

        ranked.sort_by(|a, b| {
            b.fuzzy
                .score
                .cmp(&a.fuzzy.score)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });

        ranked
    }

    /// Number of applications discovered.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.index.entries.len()
    }

    /// Whether no applications were discovered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.index.entries.is_empty()
    }
}
