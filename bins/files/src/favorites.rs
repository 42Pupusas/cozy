//! [`Favorites`] -- the user's bookmarked directories, persisted to disk.
//!
//! The sidebar's second section. Unlike the XDG places above it, this list is
//! the user's own, so it has to survive a restart: it lives in
//! `$XDG_DATA_HOME/terminal/files-favorites` (data, not config -- the user
//! edits it through the UI, not by hand, and the XDG spec puts
//! application-written state under `XDG_DATA_HOME`).
//!
//! The format is one absolute path per line. Deliberately not TOML or JSON:
//! paths are the only content, a line-based file is diffable and repairable
//! in an editor, and it cannot fail to parse in a way that loses the whole
//! list. A path containing a newline cannot be represented -- that is
//! rejected on the way in rather than silently corrupting the file.

use std::path::{Path, PathBuf};

/// An ordered, de-duplicated list of bookmarked directories.
pub struct Favorites {
    /// Insertion order is preserved: the user's own ordering is meaningful,
    /// and re-sorting it alphabetically behind their back would be rude.
    paths: Vec<PathBuf>,
}

impl Favorites {
    /// An empty list.
    #[must_use]
    pub const fn new() -> Self {
        Self { paths: Vec::new() }
    }

    /// Where the list is stored.
    #[must_use]
    pub fn storage_path() -> PathBuf {
        let base = std::env::var("XDG_DATA_HOME").map_or_else(
            |_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
                PathBuf::from(home).join(".local").join("share")
            },
            PathBuf::from,
        );
        base.join("terminal").join("files-favorites")
    }

    /// Load from the standard location, or an empty list if there is none.
    ///
    /// Never fails: a missing file is the first-run case, and an unreadable
    /// one should not stop the browser from opening. The user loses their
    /// bookmarks display, not their session.
    #[must_use]
    pub fn load() -> Self {
        let contents = std::fs::read_to_string(Self::storage_path()).unwrap_or_default();
        Self::parse(&contents)
    }

    /// Parse the file's contents: one path per line.
    #[must_use]
    pub fn parse(contents: &str) -> Self {
        let mut out = Self::new();
        for line in contents.lines() {
            let line = line.trim();
            // Skip blanks and comments so a hand-edited file with a note in
            // it still loads.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            out.add(Path::new(line));
        }
        out
    }

    /// Render back to the on-disk format.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for path in &self.paths {
            // A non-UTF-8 path cannot be written to this line-based format.
            // Dropping it keeps the rest of the list intact, which is better
            // than refusing to save at all.
            if let Some(text) = path.to_str() {
                out.push_str(text);
                out.push('\n');
            }
        }
        out
    }

    /// Persist to the standard location, creating the directory if needed.
    ///
    /// Errors are returned rather than swallowed so the caller can tell the
    /// user their bookmark did not stick -- silently losing a deliberate
    /// action is the kind of thing that makes an application feel broken.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.render())
    }

    /// Add a directory, if it is not already bookmarked.
    ///
    /// Returns whether the list changed, so the caller knows whether to
    /// repaint and re-save.
    pub fn add(&mut self, path: &Path) -> bool {
        // A newline in a path would write a line the parser reads back as two
        // separate entries, corrupting every bookmark after it.
        if path.to_str().is_some_and(|s| s.contains('\n')) {
            return false;
        }
        if self.contains(path) {
            return false;
        }
        self.paths.push(path.to_path_buf());
        true
    }

    /// Remove a directory. Returns whether anything was removed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.paths.len();
        self.paths.retain(|p| p != path);
        self.paths.len() != before
    }

    /// Add if absent, remove if present. Returns the new membership.
    pub fn toggle(&mut self, path: &Path) -> bool {
        if self.contains(path) {
            self.remove(path);
            false
        } else {
            self.add(path);
            true
        }
    }

    /// Whether `path` is bookmarked.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|p| p == path)
    }

    /// The bookmarked paths, in the user's order.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// How many bookmarks there are.
    ///
    /// Only the tests read this today -- the sidebar asks the narrower
    /// [`Self::is_empty`] -- but a collection with no way to observe its size
    /// is a collection with untestable invariants.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The label shown for a bookmark: its final component.
    ///
    /// Falls back to the full path for a root-like path with no basename, so
    /// an entry is never rendered as an empty row.
    #[must_use]
    pub fn label(path: &Path) -> String {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }
}

impl Default for Favorites {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Favorites;
    use std::path::{Path, PathBuf};

    #[test]
    fn a_fresh_list_is_empty() {
        let fav = Favorites::new();
        assert!(fav.is_empty());
        assert_eq!(fav.len(), 0);
    }

    #[test]
    fn adding_records_the_path() {
        let mut fav = Favorites::new();
        assert!(fav.add(Path::new("/tmp/project")));
        assert!(fav.contains(Path::new("/tmp/project")));
        assert_eq!(fav.len(), 1);
    }

    #[test]
    fn adding_the_same_path_twice_is_a_no_op() {
        let mut fav = Favorites::new();
        assert!(fav.add(Path::new("/tmp/project")));
        assert!(
            !fav.add(Path::new("/tmp/project")),
            "a duplicate must report no change"
        );
        assert_eq!(fav.len(), 1, "and must not appear twice");
    }

    #[test]
    fn removing_takes_it_out() {
        let mut fav = Favorites::new();
        fav.add(Path::new("/a"));
        fav.add(Path::new("/b"));
        assert!(fav.remove(Path::new("/a")));
        assert!(!fav.contains(Path::new("/a")));
        assert!(fav.contains(Path::new("/b")), "the other must survive");
    }

    #[test]
    fn removing_something_absent_reports_no_change() {
        let mut fav = Favorites::new();
        assert!(!fav.remove(Path::new("/nope")));
    }

    #[test]
    fn toggle_adds_then_removes() {
        let mut fav = Favorites::new();
        assert!(fav.toggle(Path::new("/x")), "first toggle bookmarks");
        assert!(fav.contains(Path::new("/x")));
        assert!(!fav.toggle(Path::new("/x")), "second toggle unbookmarks");
        assert!(!fav.contains(Path::new("/x")));
    }

    /// The user's ordering is their own; loading must not re-sort it.
    #[test]
    fn insertion_order_is_preserved_across_a_round_trip() {
        let mut fav = Favorites::new();
        for p in ["/zebra", "/apple", "/mango"] {
            fav.add(Path::new(p));
        }
        let reloaded = Favorites::parse(&fav.render());
        assert_eq!(
            reloaded.paths(),
            [
                PathBuf::from("/zebra"),
                PathBuf::from("/apple"),
                PathBuf::from("/mango")
            ]
        );
    }

    #[test]
    fn a_round_trip_preserves_the_list() {
        let mut fav = Favorites::new();
        fav.add(Path::new("/home/u/Code"));
        fav.add(Path::new("/mnt/data"));
        let reloaded = Favorites::parse(&fav.render());
        assert_eq!(reloaded.paths(), fav.paths());
    }

    #[test]
    fn blank_lines_and_comments_are_skipped() {
        let fav = Favorites::parse("\n# a note\n/real/path\n\n");
        assert_eq!(fav.paths(), [PathBuf::from("/real/path")]);
    }

    /// A path with an embedded newline would write two lines and the parser
    /// would read back a bogus second entry, shifting every bookmark after
    /// it. Reject it at the door.
    #[test]
    fn a_path_containing_a_newline_is_refused() {
        let mut fav = Favorites::new();
        assert!(!fav.add(Path::new("/tmp/evil\n/etc/passwd")));
        assert!(fav.is_empty());
    }

    #[test]
    fn duplicates_in_the_file_are_collapsed_on_load() {
        let fav = Favorites::parse("/same\n/same\n/other\n");
        assert_eq!(fav.len(), 2);
    }

    #[test]
    fn the_label_is_the_final_component() {
        assert_eq!(Favorites::label(Path::new("/home/u/Projects")), "Projects");
    }

    /// A path with no basename must still render as something clickable.
    #[test]
    fn a_root_path_falls_back_to_the_whole_path_for_its_label() {
        assert_eq!(Favorites::label(Path::new("/")), "/");
    }

    #[test]
    fn an_empty_document_loads_as_an_empty_list() {
        assert!(Favorites::parse("").is_empty());
    }
}
