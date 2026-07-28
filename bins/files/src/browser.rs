//! [`Browser`] -- the application state machine: a directory, a list, a
//! filter box, and the rules tying them together.
//!
//! # Why this is separate from the window
//!
//! Everything here is headless. It owns a [`DirectoryView`] (the model), a
//! [`ListPane`] (the widgets), a [`TextInput`] (the filter box), and a
//! [`Keymap`] (the policy) -- and not a single pixel or Wayland object. That
//! is what lets the interesting behaviour be tested against a real temp
//! directory in microseconds: navigating into a subdirectory, typing a filter
//! that empties the list, going up and landing back on the directory you came
//! from.
//!
//! The window above it is then reduced to what a window should be: turn
//! events into calls on this, and paint what it reports.
//!
//! # The rules that live here
//!
//! * **Filtering must re-length the pane.** The model's visible count changes
//!   on every keystroke; forget to tell the pane and the selection indexes
//!   into a list that no longer exists.
//! * **Navigating up should land on the child you came from.** Stepping out
//!   of a directory and finding the cursor at the top of a hundred siblings
//!   is the classic file-manager annoyance.
//! * **View rules carry across directories.** A user who turned on hidden
//!   files means it until they turn it off, not until they open a folder.

use fsmodel::DirectoryView;
use fsmodel::entry::Entry;
use fsmodel::reader::ReadError;
use std::path::{Path, PathBuf};
use widget::{ListCommand, ListPane, Mods, PaneResponse, TextInput};

use crate::keymap::{Action, EditAction, Focus, Keymap};

/// What the browser did with an input, for the window to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing changed; no repaint owed.
    Ignored,
    /// Visible state changed; repaint.
    Redraw,
    /// The user activated a non-directory entry. The window decides what
    /// "open" means -- this type refuses to guess, because spawning a process
    /// is a side effect a headless state machine has no business having.
    Open(PathBuf),
    /// The user asked to close the window.
    Quit,
}

impl Outcome {
    /// `Redraw` when `changed`, else `Ignored`.
    const fn redraw_if(changed: bool) -> Self {
        if changed { Self::Redraw } else { Self::Ignored }
    }
}

/// The file browser's whole state.
pub struct Browser {
    view: DirectoryView,
    pane: ListPane,
    filter: TextInput,
    keymap: Keymap,
    /// The most recent read error, shown in the status line.
    ///
    /// Kept rather than propagated: failing to enter one unreadable
    /// directory must not tear down the window, it must say so and leave the
    /// user where they were.
    error: Option<ReadError>,
}

impl Browser {
    /// Open `path` in a pane of `width` x `height` pixels with `row_height`
    /// pixel rows.
    pub fn open(path: &Path, width: i32, height: i32, row_height: i32) -> Result<Self, ReadError> {
        let view = DirectoryView::open(path)?;
        let pane = ListPane::new(width, height, row_height, view.visible_len());
        Ok(Self {
            view,
            pane,
            filter: TextInput::new(),
            keymap: Keymap::new(),
            error: None,
        })
    }

    /// The directory being shown.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.view.path()
    }

    /// The list pane, for painting.
    #[must_use]
    pub const fn pane(&self) -> &ListPane {
        &self.pane
    }

    /// The filter box, for painting.
    #[must_use]
    pub const fn filter(&self) -> &TextInput {
        &self.filter
    }

    /// Whether the filter box has focus (and so should draw a caret).
    #[must_use]
    pub const fn is_filtering(&self) -> bool {
        self.keymap.is_filtering()
    }

    /// The most recent read error, if any.
    #[must_use]
    pub const fn error(&self) -> Option<&ReadError> {
        self.error.as_ref()
    }

    /// How many entries are visible.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.view.visible_len()
    }

    /// How many entries the directory holds before filtering.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.view.total()
    }

    /// The entry at visible index `row`.
    #[must_use]
    pub fn entry(&self, row: usize) -> Option<&Entry> {
        self.view.visible_at(row)
    }

    /// The entry under the cursor.
    #[must_use]
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.pane.cursor().and_then(|row| self.entry(row))
    }

    /// Resize the pane.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.pane.resize(width, height);
    }

    /// Apply a translated key action.
    pub fn act(&mut self, action: Action) -> Outcome {
        match action {
            Action::List(cmd) => self.list_command(cmd),
            Action::Insert(c) => {
                // Typing on the list implicitly opens the filter, which is
                // what makes search-as-you-type work with no prefix key.
                self.keymap.set_focus(Focus::Filter);
                Outcome::redraw_if(self.filter.insert(c) && self.apply_query())
                    .or_redraw(self.keymap.is_filtering())
            }
            Action::Edit(edit) => {
                let changed = self.edit(edit);
                if changed {
                    self.apply_query();
                }
                Outcome::redraw_if(changed)
            }
            Action::BeginFilter => {
                let changed = !self.keymap.is_filtering();
                self.keymap.set_focus(Focus::Filter);
                Outcome::redraw_if(changed)
            }
            Action::EndFilter => {
                let changed = self.keymap.is_filtering();
                self.keymap.set_focus(Focus::List);
                Outcome::redraw_if(changed)
            }
            Action::CancelFilter => {
                // Escape both leaves the box *and* discards the query -- a
                // cancel that kept the filter would leave the list mysteriously
                // truncated with no visible box explaining why.
                let had_query = !self.filter.is_empty();
                let was_filtering = self.keymap.is_filtering();
                self.filter.clear();
                self.keymap.set_focus(Focus::List);
                if had_query {
                    self.apply_query();
                }
                Outcome::redraw_if(had_query || was_filtering)
            }
            Action::GoUp => self.go_up(),
            Action::Refresh => self.refresh(),
            Action::ToggleHidden => {
                self.view.toggle_hidden();
                self.pane.set_len(self.view.visible_len());
                Outcome::Redraw
            }
            Action::Quit => Outcome::Quit,
        }
    }

    /// Translate and apply a key press in one step.
    pub fn key(&mut self, press: platform::toplevel::KeyPress) -> Outcome {
        self.keymap
            .translate(press)
            .map_or(Outcome::Ignored, |action| self.act(action))
    }

    /// Apply an edit to the filter box, reporting whether it changed.
    fn edit(&mut self, edit: EditAction) -> bool {
        match edit {
            EditAction::Backspace => self.filter.backspace(),
            EditAction::Delete => self.filter.delete(),
            EditAction::Left => self.filter.move_left(),
            EditAction::Right => self.filter.move_right(),
            EditAction::WordLeft => self.filter.move_word_left(),
            EditAction::WordRight => self.filter.move_word_right(),
            EditAction::Home => self.filter.move_home(),
            EditAction::End => self.filter.move_end(),
            EditAction::KillToEnd => self.filter.delete_to_end(),
            EditAction::KillToStart => self.filter.delete_to_start(),
            EditAction::DeleteWordLeft => self.filter.delete_word_left(),
        }
    }

    /// Push the filter box's text into the model and re-length the pane.
    ///
    /// The re-length is the load-bearing half: the visible count changes on
    /// every keystroke, and a pane still holding the old length would let the
    /// selection index past the end of the list.
    fn apply_query(&mut self) -> bool {
        self.view.set_query(self.filter.text());
        self.pane.set_len(self.view.visible_len());
        true
    }

    /// Run a list command, resolving an activation against the model.
    fn list_command(&mut self, cmd: ListCommand) -> Outcome {
        match self.pane.command(cmd) {
            PaneResponse::Ignored => Outcome::Ignored,
            PaneResponse::Redraw => Outcome::Redraw,
            PaneResponse::Activate(row) => self.activate(row),
        }
    }

    /// Act on the entry at `row`: descend into it, or report it for opening.
    fn activate(&mut self, row: usize) -> Outcome {
        let Some(entry) = self.view.visible_at(row) else {
            return Outcome::Redraw;
        };
        if entry.is_navigable() {
            let target = entry.path.clone();
            self.navigate(&target, None)
        } else {
            Outcome::Open(entry.path.clone())
        }
    }

    /// Navigate to the parent directory, landing on the child just left.
    fn go_up(&mut self) -> Outcome {
        let Some(parent) = self.view.parent() else {
            // Already at the filesystem root: nothing above, and this must
            // not be reported as a change or the window repaints for nothing.
            return Outcome::Ignored;
        };
        let leaving = self.view.path().to_path_buf();
        self.navigate(&parent, Some(&leaving))
    }

    /// Read `path` and point the browser at it.
    ///
    /// `select` names an entry to land the cursor on -- how going up returns
    /// you to the directory you just left instead of the top of the list.
    ///
    /// View rules (hidden files, sort) are carried across deliberately: they
    /// are a property of how the user wants to browse, not of one directory.
    fn navigate(&mut self, path: &Path, select: Option<&Path>) -> Outcome {
        let filter = self.view.filter().clone();
        let sort = self.view.sort_policy();
        match DirectoryView::open_with(path, filter, sort) {
            Ok(view) => {
                self.view = view;
                self.error = None;
                // A query is scoped to the directory it was typed in: carrying
                // "report" into a folder with no matches would show an empty
                // window and look like a failed read.
                self.filter.clear();
                self.view.set_query("");
                self.keymap.set_focus(Focus::List);
                self.pane.reset(self.view.visible_len());
                if let Some(target) = select {
                    self.select_path(target);
                }
                Outcome::Redraw
            }
            Err(e) => {
                // Stay put and say so, rather than tearing the window down
                // over one unreadable directory.
                self.error = Some(e);
                Outcome::Redraw
            }
        }
    }

    /// Put the cursor on the entry whose path is `target`, if it is visible.
    fn select_path(&mut self, target: &Path) -> bool {
        let Some(row) = (0..self.view.visible_len())
            .find(|&i| self.view.visible_at(i).is_some_and(|e| e.path == target))
        else {
            return false;
        };
        // Through the pane rather than the selection directly, so the row is
        // also scrolled into view -- landing on an off-screen cursor would
        // look exactly like landing at the top.
        self.pane.select_row(row);
        true
    }

    /// Re-read the current directory, preserving the cursor where possible.
    fn refresh(&mut self) -> Outcome {
        // Captured before the read, since the row index may well name a
        // different file afterwards.
        let cursor_path = self.cursor_entry().map(|e| e.path.clone());
        match self.view.refresh() {
            Ok(()) => {
                self.error = None;
                self.pane.set_len(self.view.visible_len());
                if let Some(path) = cursor_path {
                    self.select_path(&path);
                }
                Outcome::Redraw
            }
            Err(e) => {
                self.error = Some(e);
                Outcome::Redraw
            }
        }
    }

    /// A pointer press at pane-relative coordinates.
    pub fn press(&mut self, x: i32, y: i32, mods: Mods) -> Outcome {
        // A click anywhere in the list takes focus away from the filter box,
        // so the next letter typed is a shortcut again.
        self.keymap.set_focus(Focus::List);
        match self.pane.press(x, y, mods) {
            PaneResponse::Ignored => Outcome::Ignored,
            PaneResponse::Redraw => Outcome::Redraw,
            PaneResponse::Activate(row) => self.activate(row),
        }
    }

    /// Pointer motion, for scrollbar drags.
    pub fn motion(&mut self, y: i32) -> Outcome {
        Outcome::redraw_if(self.pane.motion(y).needs_redraw())
    }

    /// Pointer release.
    pub const fn release(&mut self) -> Outcome {
        Outcome::redraw_if(self.pane.release().needs_redraw())
    }

    /// A double-click, which activates the row under the pointer.
    pub fn double_click(&mut self, x: i32, y: i32) -> Outcome {
        match self.pane.double_click(x, y) {
            PaneResponse::Activate(row) => self.activate(row),
            PaneResponse::Redraw => Outcome::Redraw,
            PaneResponse::Ignored => Outcome::Ignored,
        }
    }

    /// A wheel scroll, in lines.
    pub fn scroll(&mut self, lines: i32) -> Outcome {
        self.list_command(ListCommand::Scroll(lines))
    }
}

impl Outcome {
    /// Upgrade an `Ignored` to `Redraw` when `also` is true.
    ///
    /// Used where two independent things could each have changed and either
    /// alone owes a repaint.
    fn or_redraw(self, also: bool) -> Self {
        match self {
            Self::Ignored if also => Self::Redraw,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Browser, Outcome};
    use crate::keymap::{Action, EditAction};
    use crate::test_support::TempDir;
    use widget::{Direction, Edge, ListCommand, Mods};

    /// A pane 400x200 with 20px rows: 10 rows fit on screen.
    fn open(dir: &TempDir) -> Browser {
        Browser::open(dir.path(), 400, 200, 20).expect("open")
    }

    fn down(browser: &mut Browser) -> Outcome {
        browser.act(Action::List(ListCommand::Move(Direction::Down)))
    }

    fn activate(browser: &mut Browser) -> Outcome {
        browser.act(Action::List(ListCommand::Activate))
    }

    fn type_text(browser: &mut Browser, text: &str) {
        for c in text.chars() {
            browser.act(Action::Insert(c));
        }
    }

    /// The names currently visible, in order.
    fn names(browser: &Browser) -> Vec<String> {
        (0..browser.len())
            .filter_map(|i| browser.entry(i).map(|e| e.name.clone()))
            .collect()
    }

    #[test]
    fn opening_shows_the_directory_with_nothing_selected() {
        let dir = TempDir::new("open");
        dir.create_file("a.txt", b"x");
        dir.create_dir("sub");

        let browser = open(&dir);
        assert_eq!(browser.len(), 2);
        assert_eq!(browser.pane().cursor(), None);
        assert!(browser.error().is_none());
        assert!(!browser.is_filtering());
    }

    #[test]
    fn a_missing_directory_fails_to_open_rather_than_showing_nothing() {
        let dir = TempDir::new("open_missing");
        assert!(Browser::open(&dir.path().join("nope"), 400, 200, 20).is_err());
    }

    // -- navigation -------------------------------------------------------

    #[test]
    fn activating_a_directory_descends_into_it() {
        let dir = TempDir::new("descend");
        let sub = dir.create_dir("sub");
        dir.create_file("sub/inner.txt", b"x");

        let mut browser = open(&dir);
        down(&mut browser);
        assert_eq!(activate(&mut browser), Outcome::Redraw);
        assert_eq!(browser.path(), sub);
        assert_eq!(names(&browser), ["inner.txt"]);
    }

    /// A file must be reported for opening, never navigated into.
    #[test]
    fn activating_a_file_reports_it_for_opening() {
        let dir = TempDir::new("open_file");
        let file = dir.create_file("doc.txt", b"x");

        let mut browser = open(&dir);
        down(&mut browser);
        assert_eq!(activate(&mut browser), Outcome::Open(file));
        assert_eq!(browser.path(), dir.path(), "must not have navigated");
    }

    /// The annoyance this exists to prevent: stepping out of a directory and
    /// landing at the top of a hundred siblings instead of on the one you
    /// just left.
    #[test]
    fn going_up_lands_on_the_directory_just_left() {
        let dir = TempDir::new("go_up");
        for n in ["a_dir", "b_dir", "c_dir", "d_dir"] {
            dir.create_dir(n);
        }

        let mut browser = open(&dir);
        // Walk down to "c_dir" using real geometry rather than a guess.
        let target = names(&browser)
            .iter()
            .position(|n| n == "c_dir")
            .expect("c_dir is listed");
        for _ in 0..=target {
            down(&mut browser);
        }
        assert_eq!(
            browser.cursor_entry().map(|e| e.name.clone()),
            Some("c_dir".to_owned())
        );
        activate(&mut browser);
        assert!(browser.path().ends_with("c_dir"));

        browser.act(Action::GoUp);
        assert_eq!(browser.path(), dir.path());
        assert_eq!(
            browser.cursor_entry().map(|e| e.name.clone()),
            Some("c_dir".to_owned()),
            "the cursor must return to the directory just left"
        );
    }

    #[test]
    fn going_up_from_the_root_is_a_harmless_no_op() {
        let mut browser = Browser::open(std::path::Path::new("/"), 400, 200, 20).expect("root");
        assert_eq!(browser.act(Action::GoUp), Outcome::Ignored);
        assert_eq!(browser.path(), std::path::Path::new("/"));
    }

    /// A user who turned hidden files on means it until they turn it off --
    /// not until they open a folder.
    #[test]
    fn view_rules_carry_across_directories() {
        let dir = TempDir::new("carry_rules");
        dir.create_dir("sub");
        dir.create_file("sub/.hidden", b"x");
        dir.create_file("sub/plain.txt", b"x");

        let mut browser = open(&dir);
        browser.act(Action::ToggleHidden);
        down(&mut browser);
        activate(&mut browser);
        assert_eq!(
            browser.len(),
            2,
            "the dotfile is still shown after descending"
        );
    }

    /// A query is scoped to the directory it was typed in: carrying it along
    /// would show an empty window that looks like a failed read.
    #[test]
    fn the_filter_is_cleared_when_navigating() {
        let dir = TempDir::new("filter_scope");
        dir.create_dir("sub");
        dir.create_file("sub/inner.txt", b"x");
        dir.create_file("subtle.txt", b"x");

        let mut browser = open(&dir);
        type_text(&mut browser, "sub");
        assert!(browser.is_filtering());
        // Land on the directory and descend.
        let row = names(&browser)
            .iter()
            .position(|n| n == "sub")
            .expect("sub matches the query");
        for _ in 0..=row {
            down(&mut browser);
        }
        activate(&mut browser);

        assert!(browser.filter().is_empty(), "the query must not carry over");
        assert!(!browser.is_filtering(), "and focus returns to the list");
        assert_eq!(names(&browser), ["inner.txt"]);
    }

    // -- filtering --------------------------------------------------------

    #[test]
    fn typing_filters_the_list_as_you_go() {
        let dir = TempDir::new("filter");
        for n in ["report.pdf", "report.txt", "notes.md"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        assert_eq!(browser.len(), 3);
        type_text(&mut browser, "rep");
        assert!(browser.is_filtering());
        assert_eq!(names(&browser), ["report.pdf", "report.txt"]);
    }

    /// The coupling bug: the pane must be re-lengthed on every keystroke, or
    /// a selection can index past the end of the filtered list.
    #[test]
    fn filtering_down_to_nothing_leaves_no_stale_cursor() {
        let dir = TempDir::new("filter_empty");
        for n in ["alpha", "beta", "gamma"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        browser.act(Action::List(ListCommand::Jump(Edge::Bottom)));
        assert!(browser.pane().cursor().is_some());

        type_text(&mut browser, "zzzz");
        assert_eq!(browser.len(), 0);
        assert_eq!(
            browser.pane().cursor(),
            None,
            "no cursor into an empty list"
        );
        assert!(browser.pane().selection().is_empty());
    }

    #[test]
    fn backspacing_the_query_restores_the_list() {
        let dir = TempDir::new("filter_backspace");
        for n in ["alpha", "beta", "gamma"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        type_text(&mut browser, "al");
        assert_eq!(browser.len(), 1);
        browser.act(Action::Edit(EditAction::Backspace));
        browser.act(Action::Edit(EditAction::Backspace));
        assert_eq!(browser.len(), 3);
        assert!(browser.filter().is_empty());
    }

    /// Escape must discard the query as well as the focus: a cancel that kept
    /// filtering would leave the list mysteriously truncated with no visible
    /// box explaining why.
    #[test]
    fn cancelling_the_filter_restores_the_full_list() {
        let dir = TempDir::new("filter_cancel");
        for n in ["alpha", "beta", "gamma"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        type_text(&mut browser, "al");
        assert_eq!(browser.len(), 1);
        browser.act(Action::CancelFilter);
        assert_eq!(browser.len(), 3);
        assert!(!browser.is_filtering());
        assert!(browser.filter().is_empty());
    }

    /// Tab commits: the query stays applied but focus returns to the list, so
    /// letters are shortcuts again.
    #[test]
    fn ending_the_filter_keeps_the_query() {
        let dir = TempDir::new("filter_commit");
        for n in ["alpha", "beta"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        type_text(&mut browser, "al");
        browser.act(Action::EndFilter);
        assert!(!browser.is_filtering());
        assert_eq!(browser.len(), 1, "the query is still applied");
    }

    #[test]
    fn a_click_takes_focus_back_from_the_filter_box() {
        let dir = TempDir::new("click_focus");
        dir.create_file("alpha", b"x");

        let mut browser = open(&dir);
        type_text(&mut browser, "a");
        assert!(browser.is_filtering());
        browser.press(10, 10, Mods::none());
        assert!(!browser.is_filtering());
    }

    // -- refresh ----------------------------------------------------------

    #[test]
    fn refreshing_picks_up_a_new_file() {
        let dir = TempDir::new("refresh");
        dir.create_file("first.txt", b"x");

        let mut browser = open(&dir);
        assert_eq!(browser.len(), 1);
        dir.create_file("second.txt", b"x");
        browser.act(Action::Refresh);
        assert_eq!(browser.len(), 2);
    }

    /// A refresh happens without the user asking (an inotify event), so the
    /// cursor must stay on the *same file*, not the same row number.
    #[test]
    fn refreshing_keeps_the_cursor_on_the_same_file() {
        let dir = TempDir::new("refresh_cursor");
        for n in ["b.txt", "c.txt", "d.txt"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        down(&mut browser);
        down(&mut browser);
        let before = browser.cursor_entry().map(|e| e.name.clone());
        assert_eq!(before, Some("c.txt".to_owned()));

        // A file sorting *before* the cursor shifts every row index down by
        // one -- which is exactly what a row-based restore would get wrong.
        dir.create_file("a.txt", b"x");
        browser.act(Action::Refresh);
        assert_eq!(
            browser.cursor_entry().map(|e| e.name.clone()),
            before,
            "the cursor must follow the file, not the row number"
        );
    }

    #[test]
    fn refreshing_after_the_cursor_file_vanished_does_not_panic() {
        let dir = TempDir::new("refresh_gone");
        for n in ["a.txt", "b.txt"] {
            dir.create_file(n, b"x");
        }

        let mut browser = open(&dir);
        browser.act(Action::List(ListCommand::Jump(Edge::Bottom)));
        dir.remove("b.txt");
        browser.act(Action::Refresh);
        assert_eq!(browser.len(), 1);
        assert!(browser.pane().cursor().is_some_and(|c| c < 1));
    }

    // -- errors -----------------------------------------------------------

    /// One unreadable directory must not tear the window down; it must report
    /// and leave the user where they were.
    #[test]
    fn a_failed_navigation_reports_and_stays_put() {
        let dir = TempDir::new("nav_error");
        let sub = dir.create_dir("sub");

        let mut browser = open(&dir);
        select_named(&mut browser, "sub");
        // Remove the directory out from under the browser, mimicking a race
        // with another process.
        std::fs::remove_dir(&sub).expect("remove");
        assert_eq!(activate(&mut browser), Outcome::Redraw);
        assert!(browser.error().is_some(), "the failure must be reported");
        assert_eq!(browser.path(), dir.path(), "and we must stay put");
    }

    /// Put the cursor on the entry named `name`, deriving the row from the
    /// real listing rather than assuming a sort position.
    fn select_named(browser: &mut Browser, name: &str) {
        let row = names(browser)
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{name} is listed among {:?}", names(browser)));
        for _ in 0..=row {
            down(browser);
        }
        assert_eq!(
            browser.cursor_entry().map(|e| e.name.clone()),
            Some(name.to_owned())
        );
    }

    #[test]
    fn a_successful_navigation_clears_a_previous_error() {
        let dir = TempDir::new("clear_error");
        let sub = dir.create_dir("sub");
        dir.create_dir("other");

        let mut browser = open(&dir);
        select_named(&mut browser, "sub");
        // Removed out from under the browser, mimicking a race.
        std::fs::remove_dir(&sub).expect("remove");
        activate(&mut browser);
        assert!(browser.error().is_some(), "the dead directory must report");

        browser.act(Action::Refresh);
        select_named(&mut browser, "other");
        activate(&mut browser);
        assert!(browser.error().is_none(), "a good read clears the error");
        assert!(browser.path().ends_with("other"));
    }

    // -- quit -------------------------------------------------------------

    #[test]
    fn quit_is_reported_to_the_window() {
        let dir = TempDir::new("quit");
        let mut browser = open(&dir);
        assert_eq!(browser.act(Action::Quit), Outcome::Quit);
    }

    /// A long mixed session must never leave the selection indexing past the
    /// end of the list -- the invariant that spans model and widgets.
    #[test]
    fn a_realistic_session_never_strands_the_selection() {
        let dir = TempDir::new("session");
        for n in 0..30 {
            dir.create_file(&format!("file_{n:02}.txt"), b"xx");
        }
        dir.create_dir("sub");
        dir.create_file("sub/inner.txt", b"x");

        let mut browser = open(&dir);
        for step in 0..40 {
            match step % 7 {
                0 => drop(down(&mut browser)),
                1 => drop(browser.act(Action::List(ListCommand::Page(Direction::Down)))),
                2 => drop(browser.act(Action::Insert('f'))),
                3 => drop(browser.act(Action::Edit(EditAction::Backspace))),
                4 => drop(browser.act(Action::ToggleHidden)),
                5 => drop(browser.act(Action::Refresh)),
                _ => drop(browser.act(Action::List(ListCommand::Jump(Edge::Bottom)))),
            }
            let len = browser.len();
            assert!(
                browser
                    .pane()
                    .selection()
                    .selected()
                    .iter()
                    .all(|&r| r < len),
                "stale selection index at step {step}"
            );
            assert!(
                browser.pane().cursor().is_none_or(|c| c < len),
                "cursor past the end at step {step}"
            );
        }
    }
}
