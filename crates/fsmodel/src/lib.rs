//! Pure directory-model logic for a file browser: reading a directory into
//! typed entries, then filtering and sorting them into the list a UI
//! actually renders.
//!
//! Contains **no** Wayland, rendering, or widget knowledge, exactly as
//! `crates/launcher` contains none for `bins/launcher`. That keeps every
//! rule in here headlessly unit-testable against a real temp directory (no
//! compositor, no display server), and makes the model reusable by anything
//! that has to present a directory -- `bins/files` today, and eventually a
//! portal `FileChooser` dialog.
//!
//! # Layering
//!
//! ```text
//! DirReader  -> Listing (raw, filesystem order)
//!               |
//! Filter      -> which entries are visible
//! SortPolicy  -> what order they're in
//!               |
//! DirectoryView -> the ordered, visible slice a UI draws
//! ```
//!
//! The split matters for responsiveness: a keystroke in the filter box
//! re-runs *filter+sort* over entries already in memory, and clicking a
//! column header re-runs *sort* alone. Only an inotify event or an explicit
//! navigation costs syscalls.

pub mod entry;
pub mod filter;
pub mod kind;
pub mod reader;
pub mod sort;

#[cfg(test)]
mod test_support;

use entry::Entry;
use filter::Filter;
use reader::{DirReader, Listing, ReadError};
use sort::SortPolicy;
use std::path::{Path, PathBuf};

/// One directory, plus the view rules currently applied to it.
///
/// Owns the read→filter→sort pipeline so a UI never re-derives it: change a
/// rule, call the matching `set_*`, then read [`Self::visible`].
///
/// The visible list is materialised eagerly (rather than recomputed lazily
/// on each access) because a browser reads it many times per frame -- for
/// row hit-testing, scroll clamping, and drawing -- while it changes only on
/// an explicit state change.
#[derive(Debug, Clone)]
pub struct DirectoryView {
    listing: Listing,
    filter: Filter,
    sort: SortPolicy,
    /// Indices into `listing.entries`, filtered and ordered.
    ///
    /// Indices rather than cloned `Entry` values: a directory can hold
    /// 100k entries, and re-filtering on every keystroke must not clone
    /// every path and name it keeps.
    visible: Vec<usize>,
}

impl DirectoryView {
    /// Read `path` and build a view with default rules (hidden files off,
    /// name-ascending, directories first).
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        Self::open_with(path, Filter::default(), SortPolicy::default())
    }

    /// Read `path` and build a view with explicit rules -- used when
    /// navigating, so the user's chosen sort and hidden-files toggle carry
    /// across directories instead of resetting on every step.
    pub fn open_with(path: &Path, filter: Filter, sort: SortPolicy) -> Result<Self, ReadError> {
        let listing = DirReader::new().read(path)?;
        let mut view = Self {
            listing,
            filter,
            sort,
            visible: Vec::new(),
        };
        view.recompute();
        Ok(view)
    }

    /// The directory being shown.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.listing.path
    }

    /// How many entries were unreadable and skipped during the read.
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.listing.skipped
    }

    /// Total entries read, before filtering.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.listing.entries.len()
    }

    /// The filtered, ordered entries a UI should draw.
    #[must_use]
    pub fn visible(&self) -> Vec<&Entry> {
        self.visible
            .iter()
            .map(|&i| &self.listing.entries[i])
            .collect()
    }

    /// The visible entry at `index`, or `None` if out of range.
    ///
    /// The accessor selection/rendering should use, so a stale index (from
    /// a selection held across a refresh) can never panic.
    #[must_use]
    pub fn visible_at(&self, index: usize) -> Option<&Entry> {
        self.visible.get(index).map(|&i| &self.listing.entries[i])
    }

    /// How many entries are currently visible.
    #[must_use]
    pub const fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// Whether the filter is hiding anything.
    #[must_use]
    pub const fn is_filtered(&self) -> bool {
        self.visible.len() != self.listing.entries.len()
    }

    /// The current filter rules.
    #[must_use]
    pub const fn filter(&self) -> &Filter {
        &self.filter
    }

    /// The current sort policy.
    #[must_use]
    pub const fn sort_policy(&self) -> SortPolicy {
        self.sort
    }

    /// Replace the filter and recompute the visible list.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.recompute();
    }

    /// Replace the sort policy and recompute the visible list.
    pub fn set_sort(&mut self, sort: SortPolicy) {
        self.sort = sort;
        self.recompute();
    }

    /// Toggle hidden-file visibility (Ctrl+H).
    pub fn toggle_hidden(&mut self) {
        self.filter.toggle_hidden();
        self.recompute();
    }

    /// Set the name filter query -- called on each keystroke.
    pub fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.filter.name_contains);
        self.recompute();
    }

    /// Sort by `key`, flipping direction if it's already the active key.
    ///
    /// The standard column-header click behaviour, kept here so every call
    /// site gets it consistently rather than reimplementing the toggle.
    pub fn sort_by(&mut self, key: sort::SortKey) {
        if self.sort.key == key {
            self.sort.order = self.sort.order.toggled();
        } else {
            self.sort.key = key;
            self.sort.order = sort::SortOrder::Ascending;
        }
        self.recompute();
    }

    /// Re-read the directory from disk, preserving the current view rules.
    ///
    /// What an inotify event triggers.
    pub fn refresh(&mut self) -> Result<(), ReadError> {
        self.listing = DirReader::new().read(&self.listing.path.clone())?;
        self.recompute();
        Ok(())
    }

    /// The parent directory, or `None` at the filesystem root.
    #[must_use]
    pub fn parent(&self) -> Option<PathBuf> {
        DirReader::new().parent_of(&self.listing.path)
    }

    /// Rebuild `visible` from the current listing, filter, and sort.
    ///
    /// Sorting works on a temporary vec of references and then records the
    /// resulting indices, so the underlying `listing.entries` is never
    /// reordered -- indices into it therefore stay valid for the lifetime of
    /// the listing, which is what makes index-based visibility safe.
    fn recompute(&mut self) {
        let query = self.filter.name_contains.to_lowercase();
        let mut kept: Vec<(usize, &Entry)> = self
            .listing
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.filter.accepts_normalized(e, &query))
            .collect();

        let policy = self.sort;
        kept.sort_by(|(_, a), (_, b)| policy.compare_entries(a, b));

        self.visible = kept.into_iter().map(|(i, _)| i).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{SortKey, SortOrder};
    use crate::test_support::TempDir;

    fn names(view: &DirectoryView) -> Vec<String> {
        view.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn opens_with_defaults_hiding_dotfiles_and_grouping_dirs() {
        let dir = TempDir::new("view_defaults");
        dir.create_file("beta.txt", b"x");
        dir.create_file(".secret", b"x");
        dir.create_dir("alpha_dir");

        let view = DirectoryView::open(dir.path()).expect("open");
        assert_eq!(view.total(), 3, "all entries are read");
        assert_eq!(names(&view), ["alpha_dir", "beta.txt"]);
        assert!(view.is_filtered(), "the dotfile is hidden");
    }

    #[test]
    fn toggling_hidden_reveals_dotfiles_without_a_reread() {
        let dir = TempDir::new("view_toggle_hidden");
        dir.create_file("visible.txt", b"x");
        dir.create_file(".hidden", b"x");

        let mut view = DirectoryView::open(dir.path()).expect("open");
        assert_eq!(view.visible_len(), 1);

        view.toggle_hidden();
        assert_eq!(view.visible_len(), 2);
        assert!(!view.is_filtered());
    }

    #[test]
    fn query_narrows_the_visible_list() {
        let dir = TempDir::new("view_query");
        for n in ["report.pdf", "report.txt", "notes.md"] {
            dir.create_file(n, b"x");
        }

        let mut view = DirectoryView::open(dir.path()).expect("open");
        view.set_query("report");
        assert_eq!(names(&view), ["report.pdf", "report.txt"]);

        // Clearing restores everything -- the query must not be sticky.
        view.set_query("");
        assert_eq!(view.visible_len(), 3);
    }

    /// Clicking the same column twice flips direction; clicking a different
    /// one starts fresh at ascending.
    #[test]
    fn sort_by_toggles_direction_on_repeat_and_resets_on_change() {
        let dir = TempDir::new("view_sort_toggle");
        dir.create_file("a", b"x");
        dir.create_file("b", b"xx");

        let mut view = DirectoryView::open(dir.path()).expect("open");
        assert_eq!(view.sort_policy().order, SortOrder::Ascending);

        view.sort_by(SortKey::Name);
        assert_eq!(view.sort_policy().order, SortOrder::Descending);
        assert_eq!(names(&view), ["b", "a"]);

        view.sort_by(SortKey::Size);
        assert_eq!(view.sort_policy().key, SortKey::Size);
        assert_eq!(
            view.sort_policy().order,
            SortOrder::Ascending,
            "a new column starts ascending"
        );
    }

    #[test]
    fn refresh_picks_up_new_files() {
        let dir = TempDir::new("view_refresh");
        dir.create_file("first.txt", b"x");

        let mut view = DirectoryView::open(dir.path()).expect("open");
        assert_eq!(view.visible_len(), 1);

        dir.create_file("second.txt", b"x");
        view.refresh().expect("refresh");
        assert_eq!(view.visible_len(), 2);
    }

    /// Refreshing must not silently discard the user's sort/filter choices
    /// -- an inotify-driven refresh happens without the user asking, so a
    /// reset would look like the UI spontaneously rearranging itself.
    #[test]
    fn refresh_preserves_view_rules() {
        let dir = TempDir::new("view_refresh_rules");
        dir.create_file("a.txt", b"x");
        dir.create_file(".hidden", b"x");

        let mut view = DirectoryView::open(dir.path()).expect("open");
        view.toggle_hidden();
        view.sort_by(SortKey::Name); // -> descending

        dir.create_file("b.txt", b"x");
        view.refresh().expect("refresh");

        assert!(view.filter().show_hidden, "hidden toggle survived");
        assert_eq!(view.sort_policy().order, SortOrder::Descending);
        assert_eq!(view.visible_len(), 3);
    }

    #[test]
    fn visible_at_is_bounds_checked() {
        let dir = TempDir::new("view_bounds");
        dir.create_file("only.txt", b"x");

        let view = DirectoryView::open(dir.path()).expect("open");
        assert!(view.visible_at(0).is_some());
        // A selection index held across a refresh can outrun the list;
        // that must return None, never panic.
        assert!(view.visible_at(99).is_none());
    }

    #[test]
    fn open_propagates_read_errors() {
        let dir = TempDir::new("view_error");
        let err = DirectoryView::open(&dir.path().join("nope")).expect_err("must fail");
        assert_eq!(err, ReadError::NotFound);
    }

    #[test]
    fn parent_is_available_below_root() {
        let dir = TempDir::new("view_parent");
        let sub = dir.create_dir("child");

        let view = DirectoryView::open(&sub).expect("open");
        let parent = view.parent().expect("has parent");
        assert!(
            view.path().starts_with(&parent),
            "parent must be an ancestor of the view path"
        );
    }
}
