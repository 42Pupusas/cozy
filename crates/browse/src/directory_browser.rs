use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use fsmodel::DirectoryView;
use fsmodel::entry::Entry;
use fsmodel::reader::ReadError;
use widget::{ListCommand, ListPane, Mods, PaneResponse, TextInput};

use crate::dirs_watch::DirsWatch;
use crate::history::{History, Trail};
use crate::reaction::Reaction;

pub struct DirectoryBrowser {
    view: DirectoryView,
    pane: ListPane,
    filter: TextInput,
    history: History,
    error: Option<ReadError>,
    listing: u64,
    refreshes: u64,
    watch: DirsWatch,
}

impl DirectoryBrowser {
    pub fn open(path: &Path, width: i32, height: i32, row_height: i32) -> Result<Self, ReadError> {
        let view = DirectoryView::open(path)?;
        let pane = ListPane::new(width, height, row_height, view.visible_len());
        let mut watch = DirsWatch::new();
        watch.follow(path);
        Ok(Self {
            view,
            pane,
            filter: TextInput::new(),
            history: History::new(),
            error: None,
            listing: 0,
            refreshes: 0,
            watch,
        })
    }

    pub fn poll_listing(&mut self) -> bool {
        if !self.watch.poll() {
            return false;
        }
        self.refresh();
        true
    }

    #[must_use]
    pub fn watch_fd(&self) -> Option<RawFd> {
        self.watch.fd()
    }

    #[must_use]
    pub const fn listing_revision(&self) -> u64 {
        self.listing
    }

    #[must_use]
    pub const fn refresh_count(&self) -> u64 {
        self.refreshes
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.view.path()
    }

    #[must_use]
    pub const fn pane(&self) -> &ListPane {
        &self.pane
    }

    #[must_use]
    pub const fn filter(&self) -> &TextInput {
        &self.filter
    }

    #[must_use]
    pub const fn shows_hidden(&self) -> bool {
        self.view.filter().show_hidden
    }

    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    #[must_use]
    pub const fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    #[must_use]
    pub fn can_go_up(&self) -> bool {
        self.view.parent().is_some()
    }

    #[must_use]
    pub const fn error(&self) -> Option<&ReadError> {
        self.error.as_ref()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.view.visible_len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn entry(&self, row: usize) -> Option<&Entry> {
        self.view.visible_at(row)
    }

    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.pane.cursor().and_then(|row| self.entry(row))
    }

    #[must_use]
    pub fn path_of(&self, row: usize) -> Option<PathBuf> {
        self.entry(row).map(|entry| entry.path.clone())
    }

    #[must_use]
    pub fn name_of(&self, row: usize) -> Option<String> {
        self.entry(row).map(|entry| entry.name.clone())
    }

    #[must_use]
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let selected = self.pane.selection().selected();
        if selected.is_empty() {
            return self
                .cursor_entry()
                .map(|entry| vec![entry.path.clone()])
                .unwrap_or_default();
        }
        selected
            .iter()
            .filter_map(|&row| self.entry(row))
            .map(|entry| entry.path.clone())
            .collect()
    }

    #[must_use]
    pub fn drag_paths(&self, row: usize) -> Vec<PathBuf> {
        let selected = self.pane.selection().selected();
        if selected.contains(&row) {
            selected
                .iter()
                .filter_map(|&selected_row| self.entry(selected_row))
                .map(|entry| entry.path.clone())
                .collect()
        } else {
            self.path_of(row).into_iter().collect()
        }
    }

    #[must_use]
    pub fn navigable_row(&self, row: Option<usize>) -> Option<usize> {
        row.filter(|&row| self.entry(row).is_some_and(Entry::is_navigable))
    }

    #[must_use]
    pub fn drop_dir(&self, row: Option<usize>) -> PathBuf {
        self.navigable_row(row)
            .and_then(|row| self.entry(row))
            .map_or_else(|| self.view.path().to_path_buf(), |entry| entry.path.clone())
    }

    pub fn pick(&mut self, row: usize, mods: Mods) -> Reaction {
        Reaction::redraw_if(self.pane.pick(row, mods).needs_redraw())
    }

    pub fn steer_to(&mut self, row: usize, extend: bool) -> Reaction {
        let mods = if extend { Mods::shift() } else { Mods::none() };
        self.pick(row, mods)
    }

    pub fn activate_entry(&mut self, row: usize) -> Reaction {
        if self.pane.pick(row, Mods::none()) == PaneResponse::Ignored {
            return Reaction::Ignored;
        }
        self.activate(row)
    }

    pub fn list_command(&mut self, command: ListCommand) -> Reaction {
        match self.pane.command(command) {
            PaneResponse::Ignored => Reaction::Ignored,
            PaneResponse::Redraw => Reaction::Redraw,
            PaneResponse::Activate(row) => self.activate(row),
        }
    }

    pub fn edit_filter(&mut self, edit: impl FnOnce(&mut TextInput) -> bool) -> bool {
        let changed = edit(&mut self.filter);
        if changed {
            self.apply_query();
        }
        changed
    }

    pub fn clear_filter(&mut self) -> bool {
        let had_query = !self.filter.is_empty();
        self.filter.clear();
        if had_query {
            self.apply_query();
        }
        had_query
    }

    pub fn toggle_hidden(&mut self) -> Reaction {
        self.view.toggle_hidden();
        self.set_len(self.view.visible_len());
        Reaction::Redraw
    }

    pub fn go_back(&mut self) -> Reaction {
        let Some(target) = self.history.peek_back().map(Path::to_path_buf) else {
            return Reaction::Ignored;
        };
        let here = self.view.path().to_path_buf();
        let reaction = self.navigate(&target, None, Trail::Replay);
        if self.error.is_none() {
            self.history.commit_back(&here);
        }
        reaction
    }

    pub fn go_forward(&mut self) -> Reaction {
        let Some(target) = self.history.peek_forward().map(Path::to_path_buf) else {
            return Reaction::Ignored;
        };
        let here = self.view.path().to_path_buf();
        let reaction = self.navigate(&target, None, Trail::Replay);
        if self.error.is_none() {
            self.history.commit_forward(&here);
        }
        reaction
    }

    pub fn go_up(&mut self) -> Reaction {
        let Some(parent) = self.view.parent() else {
            return Reaction::Ignored;
        };
        let leaving = self.view.path().to_path_buf();
        self.navigate(&parent, Some(&leaving), Trail::Record)
    }

    pub fn go_to(&mut self, path: &Path) -> Reaction {
        if path == self.view.path() {
            return Reaction::Ignored;
        }
        self.navigate(path, None, Trail::Record)
    }

    pub fn refresh(&mut self) -> Reaction {
        let cursor_path = self.cursor_entry().map(|entry| entry.path.clone());
        match self.view.refresh() {
            Ok(()) => {
                self.refreshes += 1;
                self.error = None;
                self.watch.follow(self.view.path());
                self.set_len(self.view.visible_len());
                if let Some(path) = cursor_path {
                    self.select_path(&path);
                }
            }
            Err(error) => self.error = Some(error),
        }
        Reaction::Redraw
    }

    pub fn select_path(&mut self, target: &Path) -> bool {
        let Some(row) = (0..self.view.visible_len())
            .find(|&index| self.view.visible_at(index).is_some_and(|entry| entry.path == target))
        else {
            return false;
        };
        self.pane.select_row(row)
    }

    fn activate(&mut self, row: usize) -> Reaction {
        let Some(entry) = self.view.visible_at(row) else {
            return Reaction::Redraw;
        };
        if entry.is_navigable() {
            let target = entry.path.clone();
            self.navigate(&target, None, Trail::Record)
        } else {
            Reaction::Open(entry.path.clone())
        }
    }

    fn navigate(&mut self, path: &Path, select: Option<&Path>, trail: Trail) -> Reaction {
        let filter = self.view.filter().clone();
        let sort = self.view.sort_policy();
        let leaving = self.view.path().to_path_buf();
        match DirectoryView::open_with(path, filter, sort) {
            Ok(view) => {
                self.view = view;
                self.error = None;
                self.watch.follow(path);
                if trail == Trail::Record {
                    self.history.visit(&leaving);
                }
                self.filter.clear();
                self.view.set_query("");
                self.listing += 1;
                self.pane.reset(self.view.visible_len());
                if let Some(target) = select {
                    self.select_path(target);
                }
                Reaction::Moved
            }
            Err(error) => {
                self.error = Some(error);
                Reaction::Redraw
            }
        }
    }

    fn apply_query(&mut self) {
        self.view.set_query(self.filter.text());
        self.set_len(self.view.visible_len());
    }

    fn set_len(&mut self, len: usize) {
        self.listing += 1;
        self.pane.set_len(len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let dir = TempDir::new(label);
            dir.create_file("alpha.txt", b"a");
            dir.create_file("beta.txt", b"b");
            dir.create_dir("sub");
            dir.create_file("sub/inner.txt", b"i");
            dir.create_file(".hidden", b"h");
            Self { dir }
        }

        fn browser(&self) -> DirectoryBrowser {
            DirectoryBrowser::open(self.dir.path(), 400, 300, 20).expect("open")
        }

        fn row_named(browser: &DirectoryBrowser, name: &str) -> usize {
            (0..browser.len())
                .find(|&row| browser.name_of(row).as_deref() == Some(name))
                .expect("row present")
        }
    }

    #[test]
    fn opening_lists_the_visible_entries_and_hides_dotfiles() {
        let fixture = Fixture::new("open");
        let browser = fixture.browser();
        assert_eq!(browser.len(), 3);
        assert!(!browser.shows_hidden());
        assert!(browser.error().is_none());
    }

    #[test]
    fn a_missing_directory_fails_to_open() {
        let fixture = Fixture::new("missing");
        let missing = fixture.dir.path().join("nope");
        assert!(DirectoryBrowser::open(&missing, 400, 300, 20).is_err());
    }

    #[test]
    fn activating_a_directory_descends_and_reports_a_move() {
        let fixture = Fixture::new("descend");
        let mut browser = fixture.browser();
        let row = Fixture::row_named(&browser, "sub");
        assert_eq!(browser.activate_entry(row), Reaction::Moved);
        assert_eq!(browser.path(), fixture.dir.path().join("sub"));
        assert_eq!(browser.len(), 1);
    }

    #[test]
    fn activating_a_file_reports_it_for_opening() {
        let fixture = Fixture::new("open_file");
        let mut browser = fixture.browser();
        let row = Fixture::row_named(&browser, "alpha.txt");
        assert_eq!(
            browser.activate_entry(row),
            Reaction::Open(fixture.dir.path().join("alpha.txt"))
        );
    }

    #[test]
    fn going_up_lands_on_the_directory_just_left() {
        let fixture = Fixture::new("up");
        let mut browser = fixture.browser();
        let row = Fixture::row_named(&browser, "sub");
        browser.activate_entry(row);
        assert_eq!(browser.go_up(), Reaction::Moved);
        assert_eq!(browser.path(), fixture.dir.path());
        assert_eq!(browser.cursor_entry().map(|e| e.name.as_str()), Some("sub"));
    }

    #[test]
    fn back_and_forward_walk_the_trail_without_recording_themselves() {
        let fixture = Fixture::new("trail");
        let mut browser = fixture.browser();
        let row = Fixture::row_named(&browser, "sub");
        browser.activate_entry(row);
        assert!(browser.can_go_back());
        browser.go_back();
        assert_eq!(browser.path(), fixture.dir.path());
        assert!(browser.can_go_forward());
        assert!(!browser.can_go_back());
        browser.go_forward();
        assert_eq!(browser.path(), fixture.dir.path().join("sub"));
    }

    #[test]
    fn a_failed_navigation_stays_put_and_records_the_error() {
        let fixture = Fixture::new("failed");
        let mut browser = fixture.browser();
        let missing = fixture.dir.path().join("nope");
        assert_eq!(browser.go_to(&missing), Reaction::Redraw);
        assert!(browser.error().is_some());
        assert_eq!(browser.path(), fixture.dir.path());
        assert!(!browser.can_go_back());
    }

    #[test]
    fn going_to_the_open_directory_is_ignored() {
        let fixture = Fixture::new("same");
        let mut browser = fixture.browser();
        assert_eq!(browser.go_to(fixture.dir.path()), Reaction::Ignored);
    }

    #[test]
    fn typing_a_filter_relengths_the_list_and_clearing_restores_it() {
        let fixture = Fixture::new("filter");
        let mut browser = fixture.browser();
        assert!(browser.edit_filter(|input| input.insert('a')));
        assert!(browser.edit_filter(|input| input.insert('l')));
        assert_eq!(browser.len(), 1);
        assert!(browser.clear_filter());
        assert_eq!(browser.len(), 3);
        assert!(!browser.clear_filter());
    }

    #[test]
    fn a_filter_does_not_follow_the_user_into_another_directory() {
        let fixture = Fixture::new("scoped");
        let mut browser = fixture.browser();
        browser.edit_filter(|input| input.insert('s'));
        let row = Fixture::row_named(&browser, "sub");
        browser.activate_entry(row);
        assert!(browser.filter().is_empty());
        assert_eq!(browser.len(), 1);
    }

    #[test]
    fn showing_hidden_files_carries_across_directories() {
        let fixture = Fixture::new("hidden");
        let mut browser = fixture.browser();
        browser.toggle_hidden();
        assert_eq!(browser.len(), 4);
        let row = Fixture::row_named(&browser, "sub");
        browser.activate_entry(row);
        assert!(browser.shows_hidden());
    }

    #[test]
    fn selected_paths_fall_back_to_the_cursor_row() {
        let fixture = Fixture::new("selected");
        let mut browser = fixture.browser();
        assert!(browser.selected_paths().is_empty());
        let row = Fixture::row_named(&browser, "beta.txt");
        browser.pick(row, Mods::none());
        assert_eq!(browser.selected_paths(), vec![fixture.dir.path().join("beta.txt")]);
    }

    #[test]
    fn refreshing_keeps_the_cursor_on_the_same_file() {
        let fixture = Fixture::new("refresh");
        let mut browser = fixture.browser();
        let row = Fixture::row_named(&browser, "beta.txt");
        browser.pick(row, Mods::none());
        fixture.dir.create_file("aaa.txt", b"new");
        browser.refresh();
        assert_eq!(browser.len(), 4);
        assert_eq!(browser.cursor_entry().map(|e| e.name.as_str()), Some("beta.txt"));
    }

    #[test]
    fn a_file_created_elsewhere_appears_on_the_next_poll() {
        let fixture = Fixture::new("poll");
        let mut browser = fixture.browser();
        assert!(!browser.poll_listing());
        fixture.dir.create_file("late.txt", b"x");
        assert!(browser.poll_listing());
        assert_eq!(browser.len(), 4);
    }

    #[test]
    fn a_drop_lands_in_the_folder_row_or_else_the_open_directory() {
        let fixture = Fixture::new("drop");
        let browser = fixture.browser();
        let folder = Fixture::row_named(&browser, "sub");
        let file = Fixture::row_named(&browser, "alpha.txt");
        assert_eq!(browser.drop_dir(Some(folder)), fixture.dir.path().join("sub"));
        assert_eq!(browser.drop_dir(Some(file)), fixture.dir.path());
        assert_eq!(browser.drop_dir(None), fixture.dir.path());
    }
}
