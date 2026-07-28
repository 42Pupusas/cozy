//! [`Sidebar`] -- the places pane down the left edge.
//!
//! Two sections: the XDG user directories ("Places") and the user's own
//! bookmarks ("Favorites"). This module owns the *model* -- what rows exist,
//! which is highlighted, what a click resolves to -- and knows nothing about
//! pixels. `window.rs` paints it.
//!
//! The flattened-row representation is the point: a sidebar is visually two
//! lists with headings, but for hit-testing and keyboard navigation it is one
//! list of rows, some of which happen not to be selectable. Building that
//! flattening once, here, keeps the painter from re-deriving section offsets
//! and getting them subtly wrong.

use std::path::{Path, PathBuf};

use crate::favorites::Favorites;
use crate::user_dirs::UserDirs;

/// One row in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    /// A section heading -- rendered in the small caps-ish label style and
    /// never selectable.
    Heading(&'static str),
    /// A navigable place.
    Place {
        label: String,
        path: PathBuf,
        /// Whether this row came from the favorites section, so the painter
        /// can mark it and the keymap can offer "remove bookmark".
        favorite: bool,
    },
    /// Shown in place of an empty favorites list, so the section explains
    /// itself instead of appearing broken.
    Hint(&'static str),
}

impl SidebarRow {
    /// The path this row navigates to, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Place { path, .. } => Some(path.as_path()),
            Self::Heading(_) | Self::Hint(_) => None,
        }
    }

    /// Whether the row can be clicked or moved onto.
    #[must_use]
    pub const fn selectable(&self) -> bool {
        matches!(self, Self::Place { .. })
    }
}

/// The places pane: sections, rows, and which row the current directory is.
pub struct Sidebar {
    rows: Vec<SidebarRow>,
    /// Whether the pane is shown at all.
    visible: bool,
}

impl Sidebar {
    /// Heading for the XDG places section.
    const PLACES: &'static str = "Places";
    /// Heading for the bookmarks section.
    const FAVORITES: &'static str = "Favorites";
    /// Shown when nothing is bookmarked yet.
    const EMPTY_HINT: &'static str = "Ctrl+D to bookmark";

    /// Build the sidebar from the user's directories and bookmarks.
    #[must_use]
    pub fn new(dirs: &UserDirs, favorites: &Favorites) -> Self {
        let mut rows = vec![SidebarRow::Heading(Self::PLACES)];
        // Home leads the list: it is where the browser opens and where people
        // navigate back to most.
        rows.push(SidebarRow::Place {
            label: "Home".to_owned(),
            path: dirs.home().to_path_buf(),
            favorite: false,
        });
        for (dir, path) in dirs.existing() {
            // `$HOME` itself is a legal value for a user directory (it means
            // "I have no separate Desktop"); listing it again under another
            // name would be a confusing duplicate of the Home row.
            if path == dirs.home() {
                continue;
            }
            rows.push(SidebarRow::Place {
                label: dir.label().to_owned(),
                path,
                favorite: false,
            });
        }

        rows.push(SidebarRow::Heading(Self::FAVORITES));
        if favorites.is_empty() {
            rows.push(SidebarRow::Hint(Self::EMPTY_HINT));
        } else {
            for path in favorites.paths() {
                rows.push(SidebarRow::Place {
                    label: Favorites::label(path),
                    path: path.clone(),
                    favorite: true,
                });
            }
        }

        Self {
            rows,
            visible: true,
        }
    }

    /// Every row, headings included, top to bottom.
    #[must_use]
    pub fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Whether the pane is shown.
    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the pane. Returns the new visibility.
    pub const fn toggle(&mut self) -> bool {
        self.visible = !self.visible;
        self.visible
    }

    /// The row index whose path is `path`, for highlighting where we are.
    ///
    /// Prefers the favorites section on a tie: if a directory is both an XDG
    /// place and bookmarked, the bookmark is the more specific statement of
    /// intent.
    #[must_use]
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        let mut found = None;
        for (i, row) in self.rows.iter().enumerate() {
            if row.path() == Some(path) {
                let favorite = matches!(row, SidebarRow::Place { favorite: true, .. });
                if favorite {
                    return Some(i);
                }
                found = found.or(Some(i));
            }
        }
        found
    }

    /// The index of the row at a vertical offset, given a uniform row height.
    ///
    /// Returns `None` above the pane, past the last row, or on a heading --
    /// clicking a section title must do nothing rather than jump somewhere
    /// arbitrary.
    ///
    /// Yields the *index* rather than the row because the painter walks rows
    /// by position and asks "is this the hovered one", so it compares indices;
    /// a caller wanting the row itself indexes [`Self::rows`]. Both the click
    /// and the hover path go through this one function deliberately -- two
    /// copies of the arithmetic would be free to drift by a row, lighting one
    /// place while clicking another.
    #[must_use]
    pub fn row_index_at(&self, y: i32, row_height: i32) -> Option<usize> {
        if y < 0 || row_height <= 0 {
            return None;
        }
        let index = usize::try_from(y / row_height).ok()?;
        self.rows.get(index).filter(|r| r.selectable())?;
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Sidebar, SidebarRow};
    use crate::favorites::Favorites;
    use crate::test_support::TempDir;
    use crate::user_dirs::UserDirs;
    use std::path::Path;

    /// Build a sidebar over a real temp directory with a couple of XDG dirs.
    fn fixture(tmp: &TempDir) -> (UserDirs, Favorites) {
        tmp.create_dir("Documents");
        tmp.create_dir("Downloads");
        let doc = format!(
            "XDG_DOCUMENTS_DIR=\"{home}/Documents\"\nXDG_DOWNLOAD_DIR=\"{home}/Downloads\"\n",
            home = tmp.path().display()
        );
        (UserDirs::parse(&doc, tmp.path()), Favorites::new())
    }

    #[test]
    fn both_sections_are_present() {
        let tmp = TempDir::new("sidebar-sections");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        let headings: Vec<_> = bar
            .rows()
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Heading(h) => Some(*h),
                _ => None,
            })
            .collect();
        assert_eq!(headings, ["Places", "Favorites"]);
    }

    #[test]
    fn home_leads_the_places_section() {
        let tmp = TempDir::new("sidebar-home");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        let first = bar.rows().iter().find(|r| r.selectable()).expect("a place");
        assert_eq!(first.path(), Some(tmp.path()));
    }

    /// Only directories that exist may appear.
    #[test]
    fn a_missing_user_directory_is_not_listed() {
        let tmp = TempDir::new("sidebar-missing");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(
            !bar.rows()
                .iter()
                .any(|r| r.path() == Some(&tmp.path().join("Music"))),
            "Music was never created, so it must not be offered"
        );
    }

    #[test]
    fn an_empty_favorites_section_shows_a_hint_instead_of_nothing() {
        let tmp = TempDir::new("sidebar-hint");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(bar.rows().iter().any(|r| matches!(r, SidebarRow::Hint(_))));
    }

    #[test]
    fn bookmarks_appear_under_the_favorites_heading() {
        let tmp = TempDir::new("sidebar-favs");
        let (dirs, mut fav) = fixture(&tmp);
        fav.add(Path::new("/mnt/work"));
        let bar = Sidebar::new(&dirs, &fav);
        let rows = bar.rows();
        let heading = rows
            .iter()
            .position(|r| matches!(r, SidebarRow::Heading("Favorites")))
            .expect("favorites heading");
        let entry = rows
            .iter()
            .position(|r| r.path() == Some(Path::new("/mnt/work")))
            .expect("the bookmark");
        assert!(entry > heading, "a bookmark must sit under its heading");
        assert!(matches!(
            rows[entry],
            SidebarRow::Place { favorite: true, .. }
        ));
    }

    /// A heading is not a destination; clicking one must do nothing.
    #[test]
    fn a_heading_is_never_selectable() {
        let tmp = TempDir::new("sidebar-heading");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(!bar.rows()[0].selectable());
        assert_eq!(
            bar.row_index_at(0, 20),
            None,
            "clicking the heading resolves to nothing"
        );
    }

    #[test]
    fn a_click_resolves_to_the_row_under_it() {
        let tmp = TempDir::new("sidebar-click");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        // Row 1 is Home (row 0 being the "Places" heading).
        let index = bar.row_index_at(25, 20).expect("a place at row 1");
        assert_eq!(bar.rows()[index].path(), Some(tmp.path()));
    }

    #[test]
    fn a_click_past_the_last_row_resolves_to_nothing() {
        let tmp = TempDir::new("sidebar-past");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(bar.row_index_at(10_000, 20), None);
    }

    /// Degenerate geometry must not panic or divide by zero.
    #[test]
    fn a_zero_row_height_is_refused_rather_than_dividing_by_zero() {
        let tmp = TempDir::new("sidebar-zero");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert_eq!(bar.row_index_at(5, 0), None);
        assert_eq!(bar.row_index_at(-5, 20), None);
    }

    #[test]
    fn the_current_directory_is_locatable_for_highlighting() {
        let tmp = TempDir::new("sidebar-current");
        let (dirs, fav) = fixture(&tmp);
        let bar = Sidebar::new(&dirs, &fav);
        assert!(bar.index_of(tmp.path()).is_some());
        assert_eq!(bar.index_of(Path::new("/nowhere")), None);
    }

    /// A directory that is both an XDG place and a bookmark highlights as the
    /// bookmark -- the more specific statement of the user's intent.
    #[test]
    fn a_bookmarked_place_highlights_in_the_favorites_section() {
        let tmp = TempDir::new("sidebar-tie");
        let (dirs, mut fav) = fixture(&tmp);
        let docs = tmp.path().join("Documents");
        fav.add(&docs);
        let bar = Sidebar::new(&dirs, &fav);
        let index = bar.index_of(&docs).expect("found");
        assert!(matches!(
            bar.rows()[index],
            SidebarRow::Place { favorite: true, .. }
        ));
    }

    /// `XDG_DESKTOP_DIR="$HOME"` is the documented "no desktop dir" value; it
    /// must not produce a second row duplicating Home.
    #[test]
    fn a_user_directory_equal_to_home_does_not_duplicate_the_home_row() {
        let tmp = TempDir::new("sidebar-dup");
        let doc = format!("XDG_DESKTOP_DIR=\"{}\"\n", tmp.path().display());
        let dirs = UserDirs::parse(&doc, tmp.path());
        let bar = Sidebar::new(&dirs, &Favorites::new());
        let count = bar
            .rows()
            .iter()
            .filter(|r| r.path() == Some(tmp.path()))
            .count();
        assert_eq!(count, 1, "Home must appear exactly once");
    }

    #[test]
    fn toggling_hides_and_shows_the_pane() {
        let tmp = TempDir::new("sidebar-toggle");
        let (dirs, fav) = fixture(&tmp);
        let mut bar = Sidebar::new(&dirs, &fav);
        assert!(bar.visible());
        assert!(!bar.toggle());
        assert!(!bar.visible());
        assert!(bar.toggle());
    }
}
