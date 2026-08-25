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
use widget::{GridPane, ListCommand, ListPane, Mods, PaneResponse, TextInput};

use crate::clipboard::{FileClipboard, Transfer};
use crate::history::{History, Trail};
use crate::keymap::{Action, EditAction, Focus, Keymap};
use crate::password_modal::PasswordModal;
use crate::rename::Rename;

/// Which presentation is active for the current directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Metadata-rich rows.
    #[default]
    List,
    /// Square file cards.
    Grid,
}

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
    /// Bookmark (or un-bookmark) the directory currently open.
    ///
    /// Bubbled up rather than handled here for the same reason as [`Self::Open`]:
    /// the favorites list is persisted to disk, and a headless state machine
    /// has no business writing files.
    ToggleFavorite,
    /// Show or hide the places pane -- a property of the window's layout,
    /// not of the directory being browsed.
    ToggleSidebar,
    /// The user answered the password prompt, or dismissed it with `None`.
    ///
    /// Carried out rather than handled here because the reply belongs to a
    /// conversation running on another thread.
    Password(Option<String>),
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
    /// The grid keeps independent scroll geometry; selection transfers when
    /// switching views because it describes files, not their presentation.
    grid: GridPane,
    view_mode: ViewMode,
    filter: TextInput,
    keymap: Keymap,
    /// Where the user has been, for the toolbar's chevrons.
    history: History,
    /// The most recent read error, shown in the status line.
    ///
    /// Kept rather than propagated: failing to enter one unreadable
    /// directory must not tear down the window, it must say so and leave the
    /// user where they were.
    error: Option<ReadError>,
    /// Paths a Ctrl-C or Ctrl-X put aside, awaiting a paste.
    clipboard: FileClipboard,
    /// An explicit trash directory, overriding the user's home trash.
    ///
    /// `None` -- the only value in production -- resolves the home trash per
    /// operation. The override exists so the tests below can prove that
    /// Delete really trashes, without moving the developer's files into
    /// their real desktop trash to do it.
    trash: Option<fileops::Trash>,
    /// The inline rename box, when F2 has opened one.
    rename: Rename,
    /// The password prompt, when a mount has provoked one.
    password: PasswordModal,
    /// The outcome of the last file operation, shown in the status line.
    ///
    /// Separate from `error`, which describes the *directory* rather than an
    /// action taken in it: a failed paste must not read as an unreadable
    /// listing, and a successful one must not be silent -- moving twelve
    /// files with no acknowledgement leaves the user unsure it happened.
    notice: Option<String>,
}

impl Browser {
    /// Open `path` in a pane of `width` x `height` pixels with `row_height`
    /// pixel rows.
    pub fn open(path: &Path, width: i32, height: i32, row_height: i32) -> Result<Self, ReadError> {
        let view = DirectoryView::open(path)?;
        let pane = ListPane::new(width, height, row_height, view.visible_len());
        // The guide's 114px card follows the list scale, so a density or font
        // adjustment cannot make the two presentations disagree in size.
        let card = row_height.saturating_mul(114).div_euclid(49).max(1);
        let grid = GridPane::new(
            width,
            height,
            card,
            card,
            row_height / 6,
            view.visible_len(),
        );
        Ok(Self {
            view,
            pane,
            grid,
            view_mode: ViewMode::List,
            filter: TextInput::new(),
            keymap: Keymap::new(),
            history: History::new(),
            error: None,
            clipboard: FileClipboard::new(),
            trash: None,
            rename: Rename::new(),
            password: PasswordModal::new(),
            notice: None,
        })
    }

    /// The directory being shown.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.view.path()
    }

    /// The list pane, for list painting.
    #[must_use]
    pub const fn pane(&self) -> &ListPane {
        &self.pane
    }

    /// The card grid, for grid painting.
    #[must_use]
    pub const fn grid(&self) -> &GridPane {
        &self.grid
    }

    /// The presentation currently active.
    #[must_use]
    pub const fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Switch presentation, preserving the user's selected files.
    pub fn set_view_mode(&mut self, mode: ViewMode) -> Outcome {
        if self.view_mode == mode {
            return Outcome::Ignored;
        }
        match mode {
            ViewMode::Grid => self
                .grid
                .replace_selection(self.pane.selection().selected(), self.pane.cursor()),
            ViewMode::List => self
                .pane
                .replace_selection(self.grid.selection().selected(), self.grid.cursor()),
        }
        self.view_mode = mode;
        Outcome::Redraw
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

    /// Give the filter box focus, as clicking it does.
    ///
    /// Reports whether anything changed, so a click on an already-focused box
    /// does not cost a repaint.
    pub const fn focus_filter(&mut self) -> Outcome {
        let changed = !self.keymap.is_filtering();
        self.keymap.set_focus(Focus::Filter);
        Outcome::redraw_if(changed)
    }

    /// Whether dotfiles and dot-directories are currently listed.
    ///
    /// Read by the toolbar so its toggle can show the state it controls. A
    /// toggle that does not report its own state is a button that appears to
    /// do nothing every second press.
    #[must_use]
    pub const fn shows_hidden(&self) -> bool {
        self.view.filter().show_hidden
    }

    /// Whether the back chevron should be live.
    #[must_use]
    pub const fn can_go_back(&self) -> bool {
        self.history.can_go_back()
    }

    /// Whether the forward chevron should be live.
    #[must_use]
    pub const fn can_go_forward(&self) -> bool {
        self.history.can_go_forward()
    }

    /// Step back to the previously visited directory.
    pub fn go_back(&mut self) -> Outcome {
        let Some(target) = self.history.peek_back().map(Path::to_path_buf) else {
            return Outcome::Ignored;
        };
        let here = self.view.path().to_path_buf();
        let outcome = self.navigate(&target, None, Trail::Replay);
        // Committed only on success. A directory that has been unmounted since
        // it was visited must not silently consume a history step: the button
        // would appear to do nothing while quietly eating the user's trail.
        if self.error.is_none() {
            self.history.commit_back(&here);
        }
        outcome
    }

    /// Step forward to a directory a previous back left behind.
    pub fn go_forward(&mut self) -> Outcome {
        let Some(target) = self.history.peek_forward().map(Path::to_path_buf) else {
            return Outcome::Ignored;
        };
        let here = self.view.path().to_path_buf();
        let outcome = self.navigate(&target, None, Trail::Replay);
        if self.error.is_none() {
            self.history.commit_forward(&here);
        }
        outcome
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
        let cursor = match self.view_mode {
            ViewMode::List => self.pane.cursor(),
            ViewMode::Grid => self.grid.cursor(),
        };
        cursor.and_then(|row| self.entry(row))
    }

    /// The paths a drag started from `row` would carry.
    ///
    /// If `row` is part of the active selection, every selected path is
    /// dragged together -- the familiar "drag one of several selected
    /// icons and they all move" behaviour. Otherwise only `row` itself is
    /// dragged: pressing on an unselected row starts a drag of just that
    /// row without first requiring a click to select it, matching how a
    /// plain (non-selecting) press-drag works in other file managers.
    ///
    /// Empty when `row` is out of range, so a caller need not bounds-check
    /// before calling this.
    #[must_use]
    pub fn drag_paths(&self, row: usize) -> Vec<PathBuf> {
        let selected = match self.view_mode {
            ViewMode::List => self.pane.selection().selected(),
            ViewMode::Grid => self.grid.selection().selected(),
        };
        if selected.contains(&row) {
            selected
                .iter()
                .filter_map(|&r| self.entry(r))
                .map(|e| e.path.clone())
                .collect()
        } else {
            self.entry(row)
                .map(|e| vec![e.path.clone()])
                .into_iter()
                .flatten()
                .collect()
        }
    }

    /// The row (of either presentation) at pane-relative `(x, y)`, if any --
    /// the shared hit-test a drag press resolves its source row through.
    #[must_use]
    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        match self.view_mode {
            ViewMode::List => self.pane.viewport().row_at_y(y),
            ViewMode::Grid => self.grid.item_at(x, y),
        }
    }

    /// Which row, if any, a drop at pane-relative `(x, y)` would land on --
    /// and only if that row is a valid drop target (a directory, or a
    /// symlink to one). Dropping onto a plain file has no defined meaning
    /// here, so it is deliberately not reported as a target.
    #[must_use]
    pub fn drop_target_row(&self, x: i32, y: i32) -> Option<usize> {
        let row = self.row_at(x, y)?;
        self.entry(row)
            .is_some_and(Entry::is_navigable)
            .then_some(row)
    }

    /// The directory a drop at pane-relative `(x, y)` would land in: the
    /// row under the point if it is navigable (see [`Self::drop_target_row`]),
    /// otherwise the browser's own open directory -- dropping into empty
    /// space below the list is the same as dropping into the folder
    /// currently shown, exactly as every desktop file manager treats it.
    #[must_use]
    pub fn drop_target_dir(&self, x: i32, y: i32) -> PathBuf {
        self.drop_target_row(x, y)
            .and_then(|row| self.entry(row))
            .map_or_else(|| self.view.path().to_path_buf(), |e| e.path.clone())
    }

    /// Resize the pane.
    pub fn resize(&mut self, width: i32, height: i32) {
        self.pane.resize(width, height);
        self.grid.resize(width, height);
    }

    /// Apply a translated key action.
    pub fn act(&mut self, action: Action) -> Outcome {
        match action {
            Action::List(cmd) => self.list_command(cmd),
            Action::Insert(c) => {
                // The password prompt takes precedence over both: it is a
                // modal, and a character typed into it must never reach the
                // filter box, where it would be visible on screen.
                if self.password.is_open() {
                    return Outcome::redraw_if(self.password.insert(c));
                }
                // An open rename box takes the character; it is editing one
                // file's name, and re-routing to the filter would rewrite
                // the listing out from under it.
                if let Some(input) = self.rename.input_mut() {
                    return Outcome::redraw_if(input.insert(c));
                }
                // Typing on the list implicitly opens the filter, which is
                // what makes search-as-you-type work with no prefix key.
                self.keymap.set_focus(Focus::Filter);
                Outcome::redraw_if(self.filter.insert(c) && self.apply_query())
                    .or_redraw(self.keymap.is_filtering())
            }
            Action::Edit(edit) => {
                if self.password.is_open() {
                    // Backspace is the only edit the prompt accepts; the
                    // keymap sends no other, and anything else must not fall
                    // through to the filter behind the modal.
                    return Outcome::redraw_if(
                        matches!(edit, EditAction::Backspace) && self.password.backspace(),
                    );
                }
                if self.rename.is_active() {
                    return Outcome::redraw_if(self.edit_rename(edit));
                }
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
                self.set_len(self.view.visible_len());
                Outcome::Redraw
            }
            Action::ToggleFavorite => Outcome::ToggleFavorite,
            Action::ToggleSidebar => Outcome::ToggleSidebar,
            Action::Copy => self.copy_selection(),
            Action::Cut => self.cut_selection(),
            Action::Paste => self.paste(),
            Action::Trash => self.trash_selection(),
            Action::DeletePermanently => self.delete_selection(),
            Action::BeginRename => self.begin_rename(),
            Action::CommitRename => self.commit_rename(),
            Action::CancelRename => self.cancel_rename(),
            Action::SubmitPassword => self.submit_password(),
            Action::DismissPassword => self.dismiss_password(),
            Action::ClearPassword => Outcome::redraw_if(self.password.clear()),
            Action::Quit => Outcome::Quit,
        }
    }

    /// The paths the user has selected, or the cursor row when the selection
    /// is empty.
    ///
    /// The fallback is what makes the shortcuts feel right: arrowing to a
    /// file and pressing Ctrl-C must copy it, without first requiring a
    /// click or a Space to formally select it.
    #[must_use]
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let selected = match self.view_mode {
            ViewMode::List => self.pane.selection().selected(),
            ViewMode::Grid => self.grid.selection().selected(),
        };
        if selected.is_empty() {
            return self
                .cursor_entry()
                .map(|e| vec![e.path.clone()])
                .unwrap_or_default();
        }
        selected
            .iter()
            .filter_map(|&row| self.entry(row))
            .map(|e| e.path.clone())
            .collect()
    }

    /// The file clipboard, for painting cut entries dimmed.
    #[must_use]
    pub const fn clipboard(&self) -> &FileClipboard {
        &self.clipboard
    }

    /// Send trashed files to `trash` instead of the user's home trash.
    ///
    /// Test-only: the production path resolves the home trash per operation.
    #[cfg(test)]
    pub fn set_trash(&mut self, trash: fileops::Trash) {
        self.trash = Some(trash);
    }

    /// The inline rename box, for painting.
    #[must_use]
    pub const fn rename(&self) -> &Rename {
        &self.rename
    }

    /// Whether an inline rename is open.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn is_renaming(&self) -> bool {
        self.keymap.is_renaming()
    }

    /// The last file operation's result, for the status line.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Report the result of an action the window performed on the browser's
    /// behalf, such as a mount.
    pub fn set_notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    fn copy_selection(&mut self) -> Outcome {
        let paths = self.selected_paths();
        let count = paths.len();
        self.clipboard.copy(paths);
        self.notice = (count > 0).then(|| Self::describe(count, "copied"));
        Outcome::redraw_if(count > 0)
    }

    fn cut_selection(&mut self) -> Outcome {
        let paths = self.selected_paths();
        let count = paths.len();
        self.clipboard.cut(paths);
        self.notice = (count > 0).then(|| Self::describe(count, "cut"));
        Outcome::redraw_if(count > 0)
    }

    /// Transfer the clipboard's contents into the open directory.
    ///
    /// Conflicts are auto-renamed (`report (1).pdf`) rather than overwriting:
    /// a paste must never destroy a file the user did not name. A source that
    /// is already in this directory is skipped for a *move* -- it is already
    /// where it was asked to go -- but honoured for a *copy*, which is how
    /// "duplicate this file" is expressed.
    fn paste(&mut self) -> Outcome {
        let Some(pending) = self.clipboard.take_for_paste() else {
            return Outcome::Ignored;
        };
        let target = self.view.path().to_path_buf();
        let transfer = fileops::Transfer::new(
            fileops::ConflictResolver::new(fileops::ConflictPolicy::Rename),
            fileops::Progress::new(),
        );
        let mut done = 0usize;
        let mut failed = 0usize;
        for source in &pending.paths {
            let moving = pending.transfer == Transfer::Move;
            if moving && source.parent() == Some(target.as_path()) {
                continue;
            }
            let result = if moving {
                transfer.move_into(source, &target)
            } else {
                transfer.copy_into(source, &target)
            };
            match result {
                Ok(_) => done += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("files: could not paste {}: {e}", source.display());
                }
            }
        }
        let verb = if pending.transfer == Transfer::Move {
            "moved"
        } else {
            "copied"
        };
        self.notice = Some(Self::report(done, failed, verb));
        if done > 0 {
            self.refresh()
        } else {
            Outcome::Redraw
        }
    }

    /// Move the selection to the trash, from which it can be restored.
    ///
    /// A session with no `HOME` has nowhere to put it; that is reported
    /// rather than silently falling back to a permanent delete, which would
    /// turn a recoverable action into an unrecoverable one.
    fn trash_selection(&mut self) -> Outcome {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return Outcome::Ignored;
        }
        let trash = match self.trash.clone().map_or_else(fileops::Trash::home, Ok) {
            Ok(trash) => trash,
            Err(e) => {
                self.notice = Some(format!("no trash available: {e}"));
                return Outcome::Redraw;
            }
        };
        let mut done = 0usize;
        let mut failed = 0usize;
        for path in &paths {
            match trash.trash(path) {
                Ok(_) => done += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("files: could not trash {}: {e}", path.display());
                }
            }
        }
        self.after_removal(done, failed, "trashed")
    }

    /// Erase the selection outright. There is no undo for this, which is why
    /// it is bound to Shift-Delete and never to Delete alone.
    fn delete_selection(&mut self) -> Outcome {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return Outcome::Ignored;
        }
        let transfer = fileops::Transfer::new(
            fileops::ConflictResolver::default(),
            fileops::Progress::new(),
        );
        let mut done = 0usize;
        let mut failed = 0usize;
        for path in &paths {
            match transfer.delete(path) {
                Ok(()) => done += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("files: could not delete {}: {e}", path.display());
                }
            }
        }
        self.after_removal(done, failed, "deleted")
    }

    /// Shared tail of trash and delete: clear any clipboard entry naming a
    /// path that no longer exists, report, and re-read the directory.
    ///
    /// The clipboard clear is the part that is easy to forget: pasting a cut
    /// that was subsequently deleted would fail on every path, with an error
    /// that names a file the user already knows is gone.
    fn after_removal(&mut self, done: usize, failed: usize, verb: &str) -> Outcome {
        if done > 0 {
            self.clipboard.clear();
        }
        self.notice = Some(Self::report(done, failed, verb));
        if done > 0 {
            self.refresh()
        } else {
            Outcome::Redraw
        }
    }

    /// Open the rename box over the cursor row.
    fn begin_rename(&mut self) -> Outcome {
        let Some((row, path)) = self.cursor_row_and_path() else {
            return Outcome::Ignored;
        };
        if !self.rename.begin(&path, row) {
            return Outcome::Ignored;
        }
        self.keymap.set_focus(Focus::Rename);
        Outcome::Redraw
    }

    /// The cursor's row index and path together, for opening a rename.
    fn cursor_row_and_path(&self) -> Option<(usize, PathBuf)> {
        let row = match self.view_mode {
            ViewMode::List => self.pane.cursor(),
            ViewMode::Grid => self.grid.cursor(),
        }?;
        Some((row, self.entry(row)?.path.clone()))
    }

    /// Apply the typed name. An illegal name leaves the box open (see
    /// [`Rename::commit`]) so it can be corrected rather than lost.
    fn commit_rename(&mut self) -> Outcome {
        let Some(renamed) = self.rename.commit() else {
            // Either the name was refused -- box stays open -- or it was
            // unchanged, which closes the box with nothing to do.
            if self.rename.is_active() {
                return Outcome::Redraw;
            }
            self.keymap.set_focus(Focus::List);
            return Outcome::Redraw;
        };
        self.keymap.set_focus(Focus::List);
        // Refuse to clobber: renaming onto an existing name would destroy it,
        // and the user asked for a rename, not a replacement.
        if std::fs::symlink_metadata(&renamed.to).is_ok() {
            self.notice = Some(format!(
                "{} already exists",
                renamed.to.file_name().unwrap_or_default().to_string_lossy()
            ));
            return Outcome::Redraw;
        }
        match std::fs::rename(&renamed.from, &renamed.to) {
            Ok(()) => {
                self.notice = None;
                let outcome = self.refresh();
                self.select_path(&renamed.to);
                outcome
            }
            Err(e) => {
                self.notice = Some(format!("could not rename: {e}"));
                Outcome::Redraw
            }
        }
    }

    /// The password prompt, for the window to paint.
    #[must_use]
    pub const fn password(&self) -> &PasswordModal {
        &self.password
    }

    /// Open the password prompt, taking the keyboard.
    ///
    /// The focus change is the load-bearing half: without it the next
    /// keystroke would reach the file list, and the user typing a password
    /// would be issuing shortcuts against their own files.
    pub fn begin_password(&mut self, prompt: &str, echo: bool) {
        self.password.open(prompt, echo);
        self.keymap.set_focus(Focus::Password);
    }

    /// Record a notice from the authentication helper.
    pub fn set_password_notice(&mut self, message: &str) {
        self.password.set_notice(message);
    }

    /// Close the prompt and return the keyboard to the list.
    ///
    /// Used when the conversation ends on its own -- the mount finished while
    /// the prompt was still up -- so the focus cannot be stranded in a mode
    /// whose modal is gone, which would leave the browser accepting no keys.
    pub fn end_password(&mut self) -> bool {
        let closed = self.password.dismiss();
        if self.keymap.is_prompting() {
            self.keymap.set_focus(Focus::List);
        }
        closed
    }

    /// Send the typed reply, closing the prompt.
    fn submit_password(&mut self) -> Outcome {
        let Some(reply) = self.password.submit() else {
            return Outcome::Ignored;
        };
        self.keymap.set_focus(Focus::List);
        Outcome::Password(Some(reply))
    }

    /// Dismiss the prompt, answering nothing.
    fn dismiss_password(&mut self) -> Outcome {
        if !self.password.dismiss() {
            return Outcome::Ignored;
        }
        self.keymap.set_focus(Focus::List);
        Outcome::Password(None)
    }

    fn cancel_rename(&mut self) -> Outcome {
        let was_active = self.rename.is_active();
        self.rename.cancel();
        self.keymap.set_focus(Focus::List);
        Outcome::redraw_if(was_active)
    }

    /// "3 files copied", "1 file copied" -- pluralised, because "1 files"
    /// reads as a bug to anyone who sees it.
    fn describe(count: usize, verb: &str) -> String {
        let noun = if count == 1 { "item" } else { "items" };
        format!("{count} {noun} {verb}")
    }

    /// A report covering partial failure, which is the case that must not be
    /// silent: eleven of twelve files moved is not a success.
    fn report(done: usize, failed: usize, verb: &str) -> String {
        if failed == 0 {
            return Self::describe(done, verb);
        }
        if done == 0 {
            return format!("could not {} {failed}", verb.trim_end_matches("ed"));
        }
        format!("{}, {failed} failed", Self::describe(done, verb))
    }

    /// Translate and apply a key press in one step.
    pub fn key(&mut self, press: platform::toplevel::KeyPress) -> Outcome {
        self.keymap
            .translate(press)
            .map_or(Outcome::Ignored, |action| self.act(action))
    }

    /// Apply an edit to the open rename box, reporting whether it changed.
    ///
    /// A separate walk from [`Self::edit`] rather than a shared helper taking
    /// `&mut TextInput`: the two fields are reached through different
    /// borrows, and the duplication is one exhaustive match that the compiler
    /// checks, not logic that can drift.
    fn edit_rename(&mut self, edit: EditAction) -> bool {
        let Some(input) = self.rename.input_mut() else {
            return false;
        };
        match edit {
            EditAction::Backspace => input.backspace(),
            EditAction::Delete => input.delete(),
            EditAction::Left => input.move_left(),
            EditAction::Right => input.move_right(),
            EditAction::WordLeft => input.move_word_left(),
            EditAction::WordRight => input.move_word_right(),
            EditAction::Home => input.move_home(),
            EditAction::End => input.move_end(),
            EditAction::KillToEnd => input.delete_to_end(),
            EditAction::KillToStart => input.delete_to_start(),
            EditAction::DeleteWordLeft => input.delete_word_left(),
        }
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
        self.set_len(self.view.visible_len());
        true
    }

    /// Run a list command, resolving an activation against the model.
    fn list_command(&mut self, cmd: ListCommand) -> Outcome {
        match self.active_command(cmd) {
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
            self.navigate(&target, None, Trail::Record)
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
        self.navigate(&parent, Some(&leaving), Trail::Record)
    }

    /// Jump straight to `path`, as the sidebar's places do.
    ///
    /// Public counterpart to [`Self::navigate`]: a sidebar click is a
    /// navigation with no entry to re-select, and going somewhere you already
    /// are must not be reported as a change or the window repaints for
    /// nothing.
    pub fn go_to(&mut self, path: &Path) -> Outcome {
        if path == self.view.path() {
            return Outcome::Ignored;
        }
        self.navigate(path, None, Trail::Record)
    }

    /// Read `path` and point the browser at it.
    ///
    /// `select` names an entry to land the cursor on -- how going up returns
    /// you to the directory you just left instead of the top of the list.
    ///
    /// View rules (hidden files, sort) are carried across deliberately: they
    /// are a property of how the user wants to browse, not of one directory.
    fn navigate(&mut self, path: &Path, select: Option<&Path>, trail: Trail) -> Outcome {
        let filter = self.view.filter().clone();
        let sort = self.view.sort_policy();
        // Captured before the move, since `self.view` is about to be replaced.
        let leaving = self.view.path().to_path_buf();
        match DirectoryView::open_with(path, filter, sort) {
            Ok(view) => {
                self.view = view;
                self.error = None;
                // Recorded only on success, and only for a genuinely new move:
                // a back/forward step has already accounted for itself, and
                // recording it here would make the chevrons toggle between two
                // directories instead of walking the trail.
                if trail == Trail::Record {
                    self.history.visit(&leaving);
                }
                // A query is scoped to the directory it was typed in: carrying
                // "report" into a folder with no matches would show an empty
                // window and look like a failed read.
                self.filter.clear();
                self.view.set_query("");
                // A rename is scoped to the row it opened over, which no
                // longer exists in the new listing; keeping the session would
                // leave the box floating over an unrelated file.
                self.rename.cancel();
                self.notice = None;
                self.keymap.set_focus(Focus::List);
                self.pane.reset(self.view.visible_len());
                self.grid.reset(self.view.visible_len());
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
        // Through the active presentation rather than a bare selection, so
        // the target is also scrolled into view -- landing on an off-screen
        // cursor would look exactly like landing at the top.
        match self.view_mode {
            ViewMode::List => self.pane.select_row(row),
            ViewMode::Grid => self.grid.select_item(row),
        }
    }

    /// Move `paths` into `target_dir` (a real filesystem mutation via
    /// [`fileops::Transfer`]), then refresh so the change is visible at
    /// once rather than waiting for the next inotify tick.
    ///
    /// A path already inside `target_dir` -- the classic "dropped it back
    /// where it started" case -- is skipped rather than attempted: moving a
    /// file onto its own directory is a no-op the user did not intend, and
    /// `Transfer::move_into` would otherwise report a spurious conflict.
    ///
    /// Errors are collected and reported in the status line exactly like a
    /// failed navigation, rather than silently dropping any files that could
    /// not be moved -- a partially-failed drop (e.g. one file mid-flight to
    /// another user's directory) must still say so.
    pub fn accept_drop(&mut self, paths: &[PathBuf], target_dir: &Path) -> Outcome {
        let transfer = fileops::Transfer::new(
            fileops::ConflictResolver::default(),
            fileops::Progress::new(),
        );
        let mut moved_any = false;
        for source in paths {
            if source.parent() == Some(target_dir) {
                continue;
            }
            match transfer.move_into(source, target_dir) {
                Ok(_) => moved_any = true,
                Err(e) => {
                    eprintln!("files: could not move {}: {e}", source.display());
                }
            }
        }
        if moved_any {
            self.refresh()
        } else {
            Outcome::Ignored
        }
    }

    /// Re-read the current directory, preserving the cursor where possible.
    fn refresh(&mut self) -> Outcome {
        // Captured before the read, since the row index may well name a
        // different file afterwards.
        let cursor_path = self.cursor_entry().map(|e| e.path.clone());
        match self.view.refresh() {
            Ok(()) => {
                self.error = None;
                self.set_len(self.view.visible_len());
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
        // Clicking away from an open rename box commits it, the way every
        // desktop field behaves. Leaving the box open would strand it over
        // whichever row the click selected instead.
        if self.rename.is_active() {
            self.commit_rename();
        }
        // A click anywhere in the list takes focus away from the filter box,
        // so the next letter typed is a shortcut again.
        self.keymap.set_focus(Focus::List);
        match self.active_press(x, y, mods) {
            PaneResponse::Ignored => Outcome::Ignored,
            PaneResponse::Redraw => Outcome::Redraw,
            PaneResponse::Activate(row) => self.activate(row),
        }
    }

    /// Pointer motion, for scrollbar drags.
    pub fn motion(&mut self, y: i32) -> Outcome {
        Outcome::redraw_if(self.active_motion(y).needs_redraw())
    }

    /// Pointer release.
    pub const fn release(&mut self) -> Outcome {
        Outcome::redraw_if(self.active_release().needs_redraw())
    }

    /// A double-click, which activates the row under the pointer.
    pub fn double_click(&mut self, x: i32, y: i32) -> Outcome {
        match self.active_double_click(x, y) {
            PaneResponse::Activate(row) => self.activate(row),
            PaneResponse::Redraw => Outcome::Redraw,
            PaneResponse::Ignored => Outcome::Ignored,
        }
    }

    /// Keep both presentations' backing lengths consistent after a filter or refresh.
    fn set_len(&mut self, len: usize) {
        self.pane.set_len(len);
        self.grid.set_len(len);
    }

    fn active_command(&mut self, command: ListCommand) -> PaneResponse {
        match self.view_mode {
            ViewMode::List => self.pane.command(command),
            ViewMode::Grid => self.grid.command(command),
        }
    }

    fn active_press(&mut self, x: i32, y: i32, mods: Mods) -> PaneResponse {
        match self.view_mode {
            ViewMode::List => self.pane.press(x, y, mods),
            ViewMode::Grid => self.grid.press(x, y, mods),
        }
    }

    fn active_motion(&mut self, y: i32) -> PaneResponse {
        match self.view_mode {
            ViewMode::List => self.pane.motion(y),
            ViewMode::Grid => self.grid.motion(y),
        }
    }

    const fn active_release(&mut self) -> PaneResponse {
        match self.view_mode {
            ViewMode::List => self.pane.release(),
            ViewMode::Grid => self.grid.release(),
        }
    }

    fn active_double_click(&mut self, x: i32, y: i32) -> PaneResponse {
        match self.view_mode {
            ViewMode::List => self.pane.double_click(x, y),
            ViewMode::Grid => self.grid.double_click(x, y),
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

    /// Point the browser's trash at a scratch directory, so a Delete in a
    /// test never lands in the developer's real desktop trash.
    fn with_scratch_trash(browser: &mut Browser, dir: &TempDir) {
        let root = dir.path().join("scratch-trash");
        browser.set_trash(fileops::Trash::at(fileops::TrashDir::at(root)));
    }

    /// Move the cursor onto the row named `name`.
    fn cursor_to(browser: &mut Browser, name: &str) {
        for _ in 0..browser.len() {
            if browser.cursor_entry().is_some_and(|e| e.name == name) {
                return;
            }
            down(browser);
        }
        assert!(
            browser.cursor_entry().is_some_and(|e| e.name == name),
            "{name} must be reachable in the listing"
        );
    }

    /// Ctrl-C then Ctrl-V into another directory must leave the original
    /// where it was and put a copy in the target.
    #[test]
    fn copying_then_pasting_duplicates_into_the_open_directory() {
        let dir = TempDir::new("clip_copy_paste");
        dir.create_file("note.txt", b"hello");
        dir.create_dir("target");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Copy);

        browser.go_to(&dir.path().join("target"));
        browser.act(Action::Paste);

        assert!(
            dir.path().join("note.txt").exists(),
            "a copy must leave the original in place"
        );
        let pasted = dir.path().join("target/note.txt");
        assert!(pasted.exists(), "the copy must land in the open directory");
        assert_eq!(std::fs::read(&pasted).expect("read"), b"hello");
        assert_eq!(names(&browser), vec!["note.txt".to_string()]);
    }

    /// Ctrl-X then Ctrl-V must relocate: gone from the source, present in
    /// the target.
    #[test]
    fn cutting_then_pasting_moves_the_file() {
        let dir = TempDir::new("clip_cut_paste");
        dir.create_file("note.txt", b"hello");
        dir.create_dir("target");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Cut);

        browser.go_to(&dir.path().join("target"));
        browser.act(Action::Paste);

        assert!(
            !dir.path().join("note.txt").exists(),
            "a cut must remove the original"
        );
        assert!(dir.path().join("target/note.txt").exists());
    }

    /// The files must not move until the paste happens -- a cut with no
    /// paste has to leave the directory exactly as it was.
    #[test]
    fn cutting_alone_does_not_touch_the_filesystem() {
        let dir = TempDir::new("clip_cut_only");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Cut);
        assert!(
            dir.path().join("note.txt").exists(),
            "a cut is intent only until a paste lands"
        );
    }

    /// Pasting a copy into the directory it came from is how "duplicate" is
    /// expressed, and the conflict policy must rename rather than overwrite.
    #[test]
    fn pasting_a_copy_into_its_own_directory_makes_a_renamed_duplicate() {
        let dir = TempDir::new("clip_duplicate");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Copy);
        browser.act(Action::Paste);

        assert!(dir.path().join("note.txt").exists(), "the original stays");
        assert!(
            dir.path().join("note (1).txt").exists(),
            "the duplicate must be renamed, never overwrite the original"
        );
        assert_eq!(
            std::fs::read(dir.path().join("note.txt")).expect("read"),
            b"hello",
            "the original's contents must be untouched"
        );
    }

    /// The destructive case this must never become: pasting over a file of
    /// the same name has to keep both.
    #[test]
    fn pasting_over_an_existing_name_renames_instead_of_destroying_it() {
        let dir = TempDir::new("clip_no_clobber");
        dir.create_file("note.txt", b"source");
        dir.create_dir("target");
        dir.create_file("target/note.txt", b"precious");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Copy);

        browser.go_to(&dir.path().join("target"));
        browser.act(Action::Paste);

        assert_eq!(
            std::fs::read(dir.path().join("target/note.txt")).expect("read"),
            b"precious",
            "an existing file must never be overwritten by a paste"
        );
        assert_eq!(
            std::fs::read(dir.path().join("target/note (1).txt")).expect("read"),
            b"source"
        );
    }

    /// One Ctrl-C must feed several pastes; a cut must not.
    #[test]
    fn a_copy_can_be_pasted_twice_but_a_cut_cannot() {
        let dir = TempDir::new("clip_repeat");
        dir.create_file("note.txt", b"hello");
        dir.create_dir("one");
        dir.create_dir("two");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Copy);

        browser.go_to(&dir.path().join("one"));
        browser.act(Action::Paste);
        browser.go_to(&dir.path().join("two"));
        browser.act(Action::Paste);
        assert!(dir.path().join("one/note.txt").exists());
        assert!(dir.path().join("two/note.txt").exists());

        browser.go_to(dir.path());
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Cut);
        browser.go_to(&dir.path().join("one"));
        assert_eq!(
            browser.act(Action::Paste),
            Outcome::Redraw,
            "the first paste of a cut does the work"
        );
        browser.go_to(&dir.path().join("two"));
        assert_eq!(
            browser.act(Action::Paste),
            Outcome::Ignored,
            "a cut is consumed by its paste and must not run again"
        );
    }

    #[test]
    fn pasting_an_empty_clipboard_does_nothing() {
        let dir = TempDir::new("clip_empty_paste");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        assert_eq!(browser.act(Action::Paste), Outcome::Ignored);
        assert_eq!(names(&browser), vec!["note.txt".to_string()]);
    }

    /// With nothing explicitly selected, the verbs must act on the cursor
    /// row -- otherwise Ctrl-C after arrowing to a file would do nothing.
    #[test]
    fn the_clipboard_verbs_fall_back_to_the_cursor_row() {
        let dir = TempDir::new("clip_cursor_fallback");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "note.txt");
        assert_eq!(
            browser.selected_paths(),
            vec![dir.path().join("note.txt")],
            "an empty selection must fall back to the cursor"
        );
    }

    /// Delete must be recoverable: the file leaves the listing and arrives
    /// in the trash, rather than being erased.
    #[test]
    fn delete_moves_the_file_to_the_trash() {
        let dir = TempDir::new("trash_delete");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        with_scratch_trash(&mut browser, &dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Trash);

        assert!(
            !dir.path().join("note.txt").exists(),
            "the file must leave the directory"
        );
        let trashed = dir.path().join("scratch-trash/files/note.txt");
        assert!(
            trashed.exists(),
            "the file must be recoverable from the trash"
        );
        assert_eq!(std::fs::read(&trashed).expect("read"), b"hello");
    }

    #[test]
    fn shift_delete_erases_without_using_the_trash() {
        let dir = TempDir::new("perm_delete");
        dir.create_file("note.txt", b"hello");
        let mut browser = open(&dir);
        with_scratch_trash(&mut browser, &dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::DeletePermanently);

        assert!(!dir.path().join("note.txt").exists());
        assert!(
            !dir.path().join("scratch-trash/files/note.txt").exists(),
            "a permanent delete must not go through the trash"
        );
    }

    /// Pasting a cut whose source was deleted in between would fail on every
    /// path, so a removal clears the clipboard.
    #[test]
    fn deleting_a_cut_file_clears_the_clipboard() {
        let dir = TempDir::new("cut_then_delete");
        dir.create_file("note.txt", b"hello");
        dir.create_dir("target");
        let mut browser = open(&dir);
        with_scratch_trash(&mut browser, &dir);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Cut);
        cursor_to(&mut browser, "note.txt");
        browser.act(Action::Trash);

        browser.go_to(&dir.path().join("target"));
        assert_eq!(
            browser.act(Action::Paste),
            Outcome::Ignored,
            "a clipboard naming deleted files must not survive the delete"
        );
    }

    #[test]
    fn f2_renames_the_cursor_entry_on_disk() {
        let dir = TempDir::new("rename_commit");
        dir.create_file("old.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "old.txt");
        browser.act(Action::BeginRename);
        assert!(browser.is_renaming());

        browser.act(Action::Edit(EditAction::KillToStart));
        type_text(&mut browser, "new.txt");
        browser.act(Action::CommitRename);

        assert!(!browser.is_renaming(), "committing closes the box");
        assert!(!dir.path().join("old.txt").exists());
        let renamed = dir.path().join("new.txt");
        assert!(renamed.exists());
        assert_eq!(std::fs::read(&renamed).expect("read"), b"hello");
        assert_eq!(names(&browser), vec!["new.txt".to_string()]);
    }

    /// Typing during a rename must edit the name, not the filter -- a filter
    /// keystroke would re-length the list under the open box.
    #[test]
    fn typing_during_a_rename_edits_the_name_not_the_filter() {
        let dir = TempDir::new("rename_typing");
        dir.create_file("old.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "old.txt");
        browser.act(Action::BeginRename);
        type_text(&mut browser, "zz");

        assert_eq!(
            browser.rename().input().expect("open box").text(),
            "old.txtzz"
        );
        assert!(
            browser.filter().is_empty(),
            "the filter must not receive keystrokes meant for the rename box"
        );
    }

    #[test]
    fn escape_cancels_a_rename_and_leaves_the_file_alone() {
        let dir = TempDir::new("rename_cancel");
        dir.create_file("old.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "old.txt");
        browser.act(Action::BeginRename);
        browser.act(Action::Edit(EditAction::KillToStart));
        type_text(&mut browser, "new.txt");
        browser.act(Action::CancelRename);

        assert!(!browser.is_renaming());
        assert!(
            dir.path().join("old.txt").exists(),
            "a cancelled rename must not touch the file"
        );
        assert!(!dir.path().join("new.txt").exists());
    }

    /// Renaming onto a name that already exists would destroy that file.
    #[test]
    fn a_rename_onto_an_existing_name_is_refused() {
        let dir = TempDir::new("rename_clobber");
        dir.create_file("old.txt", b"source");
        dir.create_file("taken.txt", b"precious");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "old.txt");
        browser.act(Action::BeginRename);
        browser.act(Action::Edit(EditAction::KillToStart));
        type_text(&mut browser, "taken.txt");
        browser.act(Action::CommitRename);

        assert_eq!(
            std::fs::read(dir.path().join("taken.txt")).expect("read"),
            b"precious",
            "an existing file must never be clobbered by a rename"
        );
        assert!(dir.path().join("old.txt").exists());
        assert!(browser.notice().is_some_and(|n| n.contains("exists")));
    }

    /// An illegal name must leave the box open so it can be fixed, rather
    /// than silently discarding what was typed.
    #[test]
    fn committing_an_illegal_name_keeps_the_box_open() {
        let dir = TempDir::new("rename_illegal");
        dir.create_file("old.txt", b"hello");
        let mut browser = open(&dir);
        cursor_to(&mut browser, "old.txt");
        browser.act(Action::BeginRename);
        browser.act(Action::Edit(EditAction::KillToStart));
        type_text(&mut browser, "sub/new.txt");
        browser.act(Action::CommitRename);

        assert!(
            browser.is_renaming(),
            "an illegal name must leave the box open"
        );
        assert!(dir.path().join("old.txt").exists());
    }

    #[test]
    fn changing_views_keeps_the_selected_entry() {
        let dir = TempDir::new("grid_selection_transfer");
        dir.create_file("alpha.txt", b"a");
        dir.create_file("beta.txt", b"b");
        let mut browser = open(&dir);
        down(&mut browser);
        let selected = browser.cursor_entry().expect("cursor").path.clone();

        assert_eq!(
            browser.set_view_mode(super::ViewMode::Grid),
            Outcome::Redraw
        );
        assert_eq!(browser.view_mode(), super::ViewMode::Grid);
        assert_eq!(browser.cursor_entry().expect("grid cursor").path, selected);

        assert_eq!(
            browser.set_view_mode(super::ViewMode::List),
            Outcome::Redraw
        );
        assert_eq!(browser.cursor_entry().expect("list cursor").path, selected);
    }

    #[test]
    fn grid_activation_uses_the_same_directory_navigation_as_the_list() {
        let dir = TempDir::new("grid_activation");
        dir.create_dir("child");
        let mut browser = open(&dir);
        browser.set_view_mode(super::ViewMode::Grid);
        browser.press(10, 10, Mods::none());
        assert_eq!(activate(&mut browser), Outcome::Redraw);
        assert!(browser.path().ends_with("child"));
    }

    #[test]
    fn dragging_an_unselected_row_carries_only_that_row() {
        let dir = TempDir::new("drag_unselected");
        dir.create_file("a.txt", b"a");
        dir.create_file("b.txt", b"b");
        let browser = open(&dir);
        // Nothing is selected yet; dragging row 0 carries only row 0.
        let paths = browser.drag_paths(0);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], browser.entry(0).unwrap().path);
    }

    #[test]
    fn dragging_a_selected_row_carries_the_whole_selection() {
        let dir = TempDir::new("drag_selected");
        dir.create_file("a.txt", b"a");
        dir.create_file("b.txt", b"b");
        dir.create_file("c.txt", b"c");
        let mut browser = open(&dir);
        browser.act(Action::List(ListCommand::SelectAll));
        let paths = browser.drag_paths(1);
        assert_eq!(
            paths.len(),
            3,
            "dragging a selected row carries every selected path"
        );
    }

    #[test]
    fn dragging_an_out_of_range_row_yields_nothing() {
        let dir = TempDir::new("drag_out_of_range");
        let browser = open(&dir);
        assert!(browser.drag_paths(999).is_empty());
    }

    #[test]
    fn a_directory_row_is_a_valid_drop_target() {
        let dir = TempDir::new("drop_target_dir_row");
        dir.create_dir("folder");
        let browser = open(&dir);
        // Row 0 is the (only) entry, a directory, at y in [0, 20).
        assert_eq!(browser.drop_target_row(5, 5), Some(0));
    }

    #[test]
    fn a_plain_file_row_is_not_a_drop_target() {
        let dir = TempDir::new("drop_target_file_row");
        dir.create_file("a.txt", b"a");
        let browser = open(&dir);
        assert_eq!(browser.drop_target_row(5, 5), None);
    }

    #[test]
    fn empty_space_below_the_list_targets_the_open_directory() {
        let dir = TempDir::new("drop_target_empty_space");
        dir.create_file("a.txt", b"a");
        let browser = open(&dir);
        assert_eq!(browser.drop_target_dir(5, 500), browser.path());
    }

    #[test]
    fn a_directory_row_is_the_drop_target_dir() {
        let dir = TempDir::new("drop_target_dir_resolves");
        let folder = dir.create_dir("folder");
        let browser = open(&dir);
        assert_eq!(browser.drop_target_dir(5, 5), folder);
    }

    #[test]
    fn accepting_a_drop_moves_the_files_and_refreshes() {
        let dir = TempDir::new("accept_drop");
        let file = dir.create_file("a.txt", b"hello");
        let folder = dir.create_dir("folder");
        let mut browser = open(&dir);
        let outcome = browser.accept_drop(std::slice::from_ref(&file), &folder);
        assert_eq!(outcome, Outcome::Redraw);
        assert!(
            !file.exists(),
            "the source must be gone from the old location"
        );
        assert!(
            folder.join("a.txt").exists(),
            "the file must land in the target"
        );
    }

    #[test]
    fn dropping_a_file_back_into_its_own_directory_is_a_no_op() {
        let dir = TempDir::new("accept_drop_noop");
        let file = dir.create_file("a.txt", b"hello");
        let mut browser = open(&dir);
        let outcome = browser.accept_drop(std::slice::from_ref(&file), dir.path());
        assert_eq!(outcome, Outcome::Ignored);
        assert!(file.exists(), "the file must not have moved or vanished");
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

    // -- history ----------------------------------------------------------

    #[test]
    fn a_freshly_opened_browser_has_nowhere_to_go() {
        let dir = TempDir::new("history_fresh");
        let browser = open(&dir);
        assert!(
            !browser.can_go_back(),
            "the first directory has nothing behind it"
        );
        assert!(!browser.can_go_forward(), "nor anything ahead");
    }

    #[test]
    fn pressing_back_on_a_fresh_browser_costs_no_repaint() {
        // Reported as Ignored rather than Redraw: a dead button that repaints
        // the window is a wasted frame on every click.
        let dir = TempDir::new("history_dead");
        let mut browser = open(&dir);
        assert_eq!(browser.go_back(), Outcome::Ignored);
        assert_eq!(browser.go_forward(), Outcome::Ignored);
    }

    #[test]
    fn descending_then_going_back_returns_to_the_parent() {
        let dir = TempDir::new("history_descend");
        let sub = dir.create_dir("sub");

        let mut browser = open(&dir);
        down(&mut browser);
        activate(&mut browser);
        assert_eq!(browser.path(), sub);
        assert!(browser.can_go_back(), "descending must arm the back button");

        assert_eq!(browser.go_back(), Outcome::Redraw);
        assert_eq!(browser.path(), dir.path());
        assert!(
            browser.can_go_forward(),
            "going back must arm the forward button"
        );
    }

    #[test]
    fn forward_returns_to_the_directory_back_left() {
        let dir = TempDir::new("history_forward");
        let sub = dir.create_dir("sub");

        let mut browser = open(&dir);
        down(&mut browser);
        activate(&mut browser);
        browser.go_back();
        assert_eq!(browser.go_forward(), Outcome::Redraw);
        assert_eq!(browser.path(), sub);
        assert!(
            !browser.can_go_forward(),
            "the forward stack is spent once replayed"
        );
    }

    #[test]
    fn a_sidebar_jump_is_recorded_like_any_other_navigation() {
        let dir = TempDir::new("history_jump");
        let sub = dir.create_dir("sub");

        let mut browser = open(&dir);
        browser.go_to(&sub);
        assert_eq!(browser.go_back(), Outcome::Redraw);
        assert_eq!(browser.path(), dir.path());
    }

    #[test]
    fn going_up_is_recorded_so_back_can_undo_it() {
        let dir = TempDir::new("history_up");
        let sub = dir.create_dir("sub");

        let mut browser = open(&dir);
        browser.go_to(&sub);
        browser.act(Action::GoUp);
        assert_eq!(browser.path(), dir.path());
        // Back from here undoes the *up*, landing in the subdirectory again.
        assert_eq!(browser.go_back(), Outcome::Redraw);
        assert_eq!(browser.path(), sub);
    }

    #[test]
    fn back_and_forward_do_not_record_themselves() {
        // The load-bearing one. If a back step recorded itself, the chevrons
        // would toggle between two directories instead of walking the trail.
        let dir = TempDir::new("history_no_self_record");
        let one = dir.create_dir("one");
        let two = dir.create_dir("one/two");

        let mut browser = open(&dir);
        browser.go_to(&one);
        browser.go_to(&two);
        browser.go_back();
        browser.go_back();
        assert_eq!(
            browser.path(),
            dir.path(),
            "two backs must walk two steps, not oscillate"
        );
        assert!(
            !browser.can_go_back(),
            "the trail must now be exhausted rather than self-refilled"
        );
    }

    #[test]
    fn navigating_somewhere_new_abandons_the_forward_trail() {
        let dir = TempDir::new("history_branch");
        let sub = dir.create_dir("sub");
        let other = dir.create_dir("other");

        let mut browser = open(&dir);
        browser.go_to(&sub);
        browser.go_back();
        assert!(browser.can_go_forward(), "back leaves something ahead");
        browser.go_to(&other);
        assert!(
            !browser.can_go_forward(),
            "branching off must discard the abandoned forward path"
        );
    }

    #[test]
    fn a_failed_navigation_is_not_recorded_at_all() {
        // A read that failed did not move the user, so it must leave no trail:
        // otherwise back would return to the directory they never left.
        let dir = TempDir::new("history_failed");
        let mut browser = open(&dir);
        browser.go_to(&dir.path().join("nonexistent"));
        assert!(browser.error().is_some(), "the read must have failed");
        assert_eq!(browser.path(), dir.path(), "and left us where we were");
        assert!(
            !browser.can_go_back(),
            "a navigation that never happened must not be remembered"
        );
    }

    #[test]
    fn a_back_step_into_a_vanished_directory_keeps_the_trail() {
        // The peek/commit split, end to end: the directory is deleted after
        // being visited, so back fails -- and must not eat the step, or the
        // button would go dead while the user is still standing in the child.
        let dir = TempDir::new("history_vanished");
        let doomed = dir.create_dir("doomed");
        let sub = dir.create_dir("doomed/sub");

        let mut browser = open(&dir);
        browser.go_to(&doomed);
        browser.go_to(&sub);
        // The whole tree, since the parent cannot be removed while the child
        // still exists -- and a plain `remove_dir` would fail silently here,
        // leaving the read to succeed and the test to prove nothing.
        std::fs::remove_dir_all(&doomed).expect("remove the visited tree");

        browser.go_back();
        assert!(browser.error().is_some(), "the parent is gone");
        assert_eq!(browser.path(), sub, "so we stay where we were");
        assert!(browser.can_go_back(), "and the step survives to be retried");
    }

    #[test]
    fn history_survives_a_filter_being_typed() {
        // Filtering is not navigation: it must neither record a step nor clear
        // the trail.
        let dir = TempDir::new("history_filter");
        let sub = dir.create_dir("sub");
        dir.create_file("sub/report.txt", b"x");

        let mut browser = open(&dir);
        browser.go_to(&sub);
        type_text(&mut browser, "rep");
        assert!(
            browser.can_go_back(),
            "typing a filter must not discard the trail"
        );
        browser.go_back();
        assert_eq!(browser.path(), dir.path());
    }

    #[test]
    fn clicking_the_filter_box_focuses_it_once() {
        let dir = TempDir::new("focus_filter");
        let mut browser = open(&dir);
        assert_eq!(browser.focus_filter(), Outcome::Redraw);
        assert!(browser.is_filtering(), "the box must now have focus");
        assert_eq!(
            browser.focus_filter(),
            Outcome::Ignored,
            "clicking an already-focused box owes no repaint"
        );
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
