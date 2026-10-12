use std::path::{Path, PathBuf};

use browse::{BrowseAction, DirectoryBrowser, Reaction};
use fsmodel::entry::Entry;
use platform::toplevel::KeyPress;
use portal_core::{ChooserRequest, Selection};
use widget::{ListCommand, Mods};

use crate::chooser_action::ChooserAction;
use crate::chooser_keys::ChooserKeys;
use crate::chooser_step::ChooserStep;
use crate::pick_mode::PickMode;
use crate::start_folder::StartFolder;

pub struct ChooserModel {
    nav: DirectoryBrowser,
    mode: PickMode,
    filtering: bool,
}

impl ChooserModel {
    const PANE_WIDTH: i32 = 400;
    const PANE_HEIGHT: i32 = 300;
    const ROW_HEIGHT: i32 = 20;

    /// # Errors
    ///
    /// Returns a message when the request is not an open request or the start folder cannot be read.
    pub fn open(request: &ChooserRequest) -> Result<Self, String> {
        let start = StartFolder::from_environment(request.current_folder.as_deref());
        Self::open_at(request, &start)
    }

    /// # Errors
    ///
    /// Returns a message when the request is not an open request or `start` cannot be read.
    pub fn open_at(request: &ChooserRequest, start: &Path) -> Result<Self, String> {
        let mode = PickMode::from_kind(&request.kind)
            .ok_or_else(|| "only open requests are supported yet".to_owned())?;
        let nav =
            DirectoryBrowser::open(start, Self::PANE_WIDTH, Self::PANE_HEIGHT, Self::ROW_HEIGHT)
                .map_err(|error| format!("{}: {error}", start.display()))?;
        Ok(Self {
            nav,
            mode,
            filtering: false,
        })
    }

    #[must_use]
    pub const fn navigator(&self) -> &DirectoryBrowser {
        &self.nav
    }

    #[must_use]
    pub const fn mode(&self) -> PickMode {
        self.mode
    }

    #[must_use]
    pub const fn is_filtering(&self) -> bool {
        self.filtering
    }

    pub fn poll_listing(&mut self) -> ChooserStep {
        ChooserStep::redraw_if(self.nav.poll_listing())
    }

    #[must_use]
    pub const fn translate(&self, press: KeyPress) -> Option<ChooserAction> {
        ChooserKeys::translate(self.filtering, press)
    }

    pub fn go_up(&mut self) -> ChooserStep {
        let reaction = self.nav.go_up();
        self.step_of(&reaction)
    }

    pub fn key(&mut self, press: KeyPress) -> ChooserStep {
        ChooserKeys::translate(self.filtering, press)
            .map_or(ChooserStep::Ignored, |action| self.act(action))
    }

    pub fn act(&mut self, action: ChooserAction) -> ChooserStep {
        match action {
            ChooserAction::Accept => self.accept(),
            ChooserAction::Cancel => ChooserStep::Cancelled,
            ChooserAction::Browse(action) => self.browse(action),
        }
    }

    pub fn pick(&mut self, row: usize, mods: Mods) -> ChooserStep {
        if self.mode.wants_directories() && !self.is_folder(row) {
            return ChooserStep::Ignored;
        }
        self.filtering = false;
        let reaction = self.nav.pick(row, self.allowed(mods));
        self.step_of(&reaction)
    }

    pub fn steer_to(&mut self, row: usize, extend: bool) -> ChooserStep {
        let reaction = self.nav.steer_to(row, extend && self.mode.multiple());
        self.step_of(&reaction)
    }

    pub fn activate_entry(&mut self, row: usize) -> ChooserStep {
        if self.mode.wants_directories() && !self.is_folder(row) {
            return ChooserStep::Ignored;
        }
        let reaction = self.nav.activate_entry(row);
        self.step_of(&reaction)
    }

    pub fn clear_selection(&mut self) -> ChooserStep {
        self.filtering = false;
        self.list_command(ListCommand::ClearSelection)
    }

    pub fn list_command(&mut self, command: ListCommand) -> ChooserStep {
        match command {
            ListCommand::Activate if self.nav.pane().cursor().is_none() && self.nav.len() == 1 => {
                self.activate_entry(0)
            }
            ListCommand::SelectAll if !self.mode.multiple() => ChooserStep::Ignored,
            ListCommand::Extend(direction) if !self.mode.multiple() => {
                let reaction = self.nav.list_command(ListCommand::Move(direction));
                self.step_of(&reaction)
            }
            _ => {
                let reaction = self.nav.list_command(command);
                self.step_of(&reaction)
            }
        }
    }

    #[must_use]
    pub fn can_accept(&self) -> bool {
        !self.accepted_paths().is_empty()
    }

    #[must_use]
    pub const fn accept_label(&self) -> &'static str {
        if self.mode.wants_directories() {
            "Select"
        } else {
            "Open"
        }
    }

    pub fn accept(&mut self) -> ChooserStep {
        let paths = self.accepted_paths();
        if paths.is_empty() {
            return ChooserStep::Ignored;
        }
        ChooserStep::Finished(Selection {
            paths,
            ..Selection::default()
        })
    }

    fn accepted_paths(&self) -> Vec<PathBuf> {
        if self.mode.wants_directories() {
            self.accepted_directories()
        } else {
            self.accepted_files()
        }
    }

    fn browse(&mut self, action: BrowseAction) -> ChooserStep {
        match action {
            BrowseAction::List(command) => self.list_command(command),
            BrowseAction::Insert(c) => {
                self.filtering = true;
                self.nav.edit_filter(|input| input.insert(c));
                ChooserStep::Redraw
            }
            BrowseAction::Edit(edit) => {
                ChooserStep::redraw_if(self.nav.edit_filter(|input| edit.apply(input)))
            }
            BrowseAction::BeginFilter => {
                let changed = !self.filtering;
                self.filtering = true;
                ChooserStep::redraw_if(changed)
            }
            BrowseAction::EndFilter => {
                let changed = self.filtering;
                self.filtering = false;
                ChooserStep::redraw_if(changed)
            }
            BrowseAction::CancelFilter => {
                let changed = self.filtering;
                self.filtering = false;
                ChooserStep::redraw_if(self.nav.clear_filter() || changed)
            }
            BrowseAction::GoUp => {
                let reaction = self.nav.go_up();
                self.step_of(&reaction)
            }
            BrowseAction::Refresh => {
                let reaction = self.nav.refresh();
                self.step_of(&reaction)
            }
            BrowseAction::ToggleHidden => {
                let reaction = self.nav.toggle_hidden();
                self.step_of(&reaction)
            }
        }
    }

    fn step_of(&mut self, reaction: &Reaction) -> ChooserStep {
        match reaction {
            Reaction::Ignored => ChooserStep::Ignored,
            Reaction::Redraw => ChooserStep::Redraw,
            Reaction::Moved => {
                self.filtering = false;
                ChooserStep::Moved
            }
            Reaction::Open(_) if self.mode.wants_directories() => ChooserStep::Ignored,
            Reaction::Open(_) => self.accept(),
        }
    }

    const fn allowed(&self, mods: Mods) -> Mods {
        if self.mode.multiple() {
            mods
        } else {
            Mods::none()
        }
    }

    fn is_folder(&self, row: usize) -> bool {
        self.nav.entry(row).is_some_and(Entry::is_navigable)
    }

    fn chosen_rows(&self) -> Vec<usize> {
        let selected = self.nav.pane().selection().selected();
        if !selected.is_empty() {
            return selected.to_vec();
        }
        self.nav.pane().cursor().into_iter().collect()
    }

    fn chosen_paths(&self, folders: bool) -> Vec<PathBuf> {
        let rows = self.chosen_rows();
        let rows = if self.mode.multiple() {
            rows
        } else {
            self.nav.pane().cursor().into_iter().collect()
        };
        rows.into_iter()
            .filter(|&row| self.is_folder(row) == folders)
            .filter_map(|row| self.nav.path_of(row))
            .collect()
    }

    fn accepted_files(&self) -> Vec<PathBuf> {
        self.chosen_paths(false)
    }

    fn accepted_directories(&self) -> Vec<PathBuf> {
        if self.chosen_rows().is_empty() {
            return vec![self.nav.path().to_path_buf()];
        }
        self.chosen_paths(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ScratchDir;
    use platform::toplevel::{Key, ModifierState};
    use portal_core::ChooserKind;
    use widget::Direction;

    struct Setup {
        dir: ScratchDir,
    }

    impl Setup {
        fn new(label: &str) -> Self {
            let dir = ScratchDir::new(label);
            dir.file("alpha.txt");
            dir.file("beta.txt");
            dir.file(".hidden");
            dir.folder("sub");
            dir.file("sub/inner.txt");
            dir.folder("other");
            Self { dir }
        }

        fn request(kind: ChooserKind) -> ChooserRequest {
            ChooserRequest {
                app_id: String::new(),
                parent_window: String::new(),
                title: String::new(),
                kind,
                accept_label: None,
                modal: true,
                filters: vec![],
                current_filter: None,
                choices: vec![],
                current_folder: None,
            }
        }

        fn model(&self, kind: ChooserKind) -> ChooserModel {
            ChooserModel::open_at(&Self::request(kind), self.dir.path()).expect("model")
        }

        fn files(&self, multiple: bool) -> ChooserModel {
            self.model(ChooserKind::Open {
                multiple,
                directory: false,
            })
        }

        fn folders(&self, multiple: bool) -> ChooserModel {
            self.model(ChooserKind::Open {
                multiple,
                directory: true,
            })
        }

        fn row(model: &ChooserModel, name: &str) -> usize {
            (0..model.navigator().len())
                .find(|&row| model.navigator().name_of(row).as_deref() == Some(name))
                .expect("row present")
        }

        fn press(key: Key) -> KeyPress {
            KeyPress {
                key,
                modifiers: ModifierState::from_wire(0, 0, 0, 0),
                repeat: false,
            }
        }

        fn ctrl_press(key: Key) -> KeyPress {
            KeyPress {
                key,
                modifiers: ModifierState::from_wire(1 << 2, 0, 0, 0),
                repeat: false,
            }
        }

        fn paths(step: &ChooserStep) -> Vec<PathBuf> {
            match step {
                ChooserStep::Finished(selection) => selection.paths.clone(),
                other => panic!("expected a selection, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_save_request_is_refused() {
        let setup = Setup::new("save_refused");
        let request = Setup::request(ChooserKind::Save {
            name: None,
            file: None,
        });
        assert!(ChooserModel::open_at(&request, setup.dir.path()).is_err());
    }

    #[test]
    fn a_missing_start_folder_is_refused() {
        let setup = Setup::new("missing_start");
        let request = Setup::request(ChooserKind::Open {
            multiple: false,
            directory: false,
        });
        let missing = setup.dir.path().join("nope");
        assert!(ChooserModel::open_at(&request, &missing).is_err());
    }

    #[test]
    fn opening_lists_the_requested_folder() {
        let setup = Setup::new("lists");
        let model = setup.files(false);
        assert_eq!(model.navigator().path(), setup.dir.path());
        assert_eq!(model.navigator().len(), 4);
    }

    #[test]
    fn double_clicking_a_file_picks_it() {
        let setup = Setup::new("double_file");
        let mut model = setup.files(false);
        let row = Setup::row(&model, "alpha.txt");
        let step = model.activate_entry(row);
        assert_eq!(
            Setup::paths(&step),
            vec![setup.dir.path().join("alpha.txt")]
        );
    }

    #[test]
    fn double_clicking_a_folder_enters_it() {
        let setup = Setup::new("double_folder");
        let mut model = setup.files(false);
        let row = Setup::row(&model, "sub");
        assert_eq!(model.activate_entry(row), ChooserStep::Moved);
        assert_eq!(model.navigator().path(), setup.dir.path().join("sub"));
    }

    #[test]
    fn enter_on_a_file_picks_it_and_on_a_folder_enters_it() {
        let setup = Setup::new("enter");
        let mut model = setup.files(false);
        model.pick(Setup::row(&model, "sub"), Mods::none());
        assert_eq!(model.key(Setup::press(Key::Enter)), ChooserStep::Moved);
        model.pick(0, Mods::none());
        let step = model.key(Setup::press(Key::Enter));
        assert_eq!(
            Setup::paths(&step),
            vec![setup.dir.path().join("sub/inner.txt")]
        );
    }

    #[test]
    fn accepting_with_a_folder_selected_in_file_mode_does_nothing() {
        let setup = Setup::new("accept_folder");
        let mut model = setup.files(false);
        model.pick(Setup::row(&model, "sub"), Mods::none());
        assert_eq!(model.accept(), ChooserStep::Ignored);
    }

    #[test]
    fn accepting_with_nothing_selected_does_nothing_in_file_mode() {
        let setup = Setup::new("accept_nothing");
        let mut model = setup.files(false);
        assert_eq!(model.accept(), ChooserStep::Ignored);
    }

    #[test]
    fn multiple_mode_returns_every_selected_file_and_skips_folders() {
        let setup = Setup::new("multi");
        let mut model = setup.files(true);
        model.list_command(ListCommand::SelectAll);
        let mut paths = Setup::paths(&model.accept());
        paths.sort();
        assert_eq!(
            paths,
            vec![
                setup.dir.path().join("alpha.txt"),
                setup.dir.path().join("beta.txt")
            ]
        );
    }

    #[test]
    fn enter_in_multiple_mode_returns_the_whole_selection() {
        let setup = Setup::new("multi_enter");
        let mut model = setup.files(true);
        model.pick(Setup::row(&model, "alpha.txt"), Mods::none());
        model.pick(Setup::row(&model, "beta.txt"), Mods::ctrl());
        let step = model.key(Setup::press(Key::Enter));
        assert_eq!(Setup::paths(&step).len(), 2);
    }

    #[test]
    fn single_mode_ignores_select_all_and_ctrl_clicks() {
        let setup = Setup::new("single_guard");
        let mut model = setup.files(false);
        assert_eq!(
            model.list_command(ListCommand::SelectAll),
            ChooserStep::Ignored
        );
        model.pick(Setup::row(&model, "alpha.txt"), Mods::none());
        model.pick(Setup::row(&model, "beta.txt"), Mods::ctrl());
        let paths = Setup::paths(&model.accept());
        assert_eq!(paths, vec![setup.dir.path().join("beta.txt")]);
    }

    #[test]
    fn single_mode_extending_moves_instead_of_selecting_a_range() {
        let setup = Setup::new("single_extend");
        let mut model = setup.files(false);
        model.pick(0, Mods::none());
        model.list_command(ListCommand::Extend(Direction::Down));
        assert_eq!(model.navigator().pane().selection().selected().len(), 1);
    }

    #[test]
    fn directory_mode_accepts_the_open_folder_when_nothing_is_selected() {
        let setup = Setup::new("dir_current");
        let mut model = setup.folders(false);
        let step = model.accept();
        assert_eq!(Setup::paths(&step), vec![setup.dir.path().to_path_buf()]);
    }

    #[test]
    fn directory_mode_accepts_the_selected_folder() {
        let setup = Setup::new("dir_selected");
        let mut model = setup.folders(false);
        model.pick(Setup::row(&model, "sub"), Mods::none());
        let step = model.accept();
        assert_eq!(Setup::paths(&step), vec![setup.dir.path().join("sub")]);
    }

    #[test]
    fn directory_mode_will_not_activate_a_file() {
        let setup = Setup::new("dir_no_activate");
        let mut model = setup.folders(false);
        let step = model.activate_entry(Setup::row(&model, "alpha.txt"));
        assert_eq!(step, ChooserStep::Ignored);
        assert!(model.navigator().pane().selection().selected().is_empty());
    }

    #[test]
    fn accepting_is_possible_exactly_when_something_would_be_returned() {
        let setup = Setup::new("can_accept");
        let mut files = setup.files(false);
        assert!(!files.can_accept());
        files.pick(Setup::row(&files, "alpha.txt"), Mods::none());
        assert!(files.can_accept());
        files.pick(Setup::row(&files, "sub"), Mods::none());
        assert!(!files.can_accept());
        assert!(setup.folders(false).can_accept());
    }

    #[test]
    fn the_accept_label_names_what_is_being_chosen() {
        let setup = Setup::new("accept_label");
        assert_eq!(setup.files(false).accept_label(), "Open");
        assert_eq!(setup.folders(false).accept_label(), "Select");
    }

    #[test]
    fn directory_mode_will_not_select_a_file() {
        let setup = Setup::new("dir_no_file");
        let mut model = setup.folders(false);
        let step = model.pick(Setup::row(&model, "alpha.txt"), Mods::none());
        assert_eq!(step, ChooserStep::Ignored);
    }

    #[test]
    fn directory_mode_enter_descends_and_never_picks_a_file() {
        let setup = Setup::new("dir_enter");
        let mut model = setup.folders(false);
        model.pick(Setup::row(&model, "sub"), Mods::none());
        assert_eq!(model.key(Setup::press(Key::Enter)), ChooserStep::Moved);
        model.steer_to(0, false);
        assert_eq!(model.key(Setup::press(Key::Enter)), ChooserStep::Ignored);
    }

    #[test]
    fn directory_mode_after_descending_accepts_the_new_folder() {
        let setup = Setup::new("dir_after_descend");
        let mut model = setup.folders(false);
        model.pick(Setup::row(&model, "sub"), Mods::none());
        model.key(Setup::press(Key::Enter));
        let step = model.accept();
        assert_eq!(Setup::paths(&step), vec![setup.dir.path().join("sub")]);
    }

    #[test]
    fn directory_mode_with_only_a_file_cursor_does_not_accept() {
        let setup = Setup::new("dir_file_cursor");
        let mut model = setup.folders(false);
        model.steer_to(Setup::row(&model, "alpha.txt"), false);
        assert_eq!(model.accept(), ChooserStep::Ignored);
    }

    #[test]
    fn multiple_directory_mode_returns_every_selected_folder() {
        let setup = Setup::new("dir_multi");
        let mut model = setup.folders(true);
        model.list_command(ListCommand::SelectAll);
        let mut paths = Setup::paths(&model.accept());
        paths.sort();
        assert_eq!(
            paths,
            vec![setup.dir.path().join("other"), setup.dir.path().join("sub")]
        );
    }

    #[test]
    fn escape_cancels_from_the_list() {
        let setup = Setup::new("escape");
        let mut model = setup.files(false);
        assert_eq!(model.key(Setup::press(Key::Escape)), ChooserStep::Cancelled);
    }

    #[test]
    fn control_enter_accepts() {
        let setup = Setup::new("ctrl_enter");
        let mut model = setup.files(false);
        model.pick(Setup::row(&model, "beta.txt"), Mods::none());
        let step = model.key(Setup::ctrl_press(Key::Enter));
        assert_eq!(Setup::paths(&step), vec![setup.dir.path().join("beta.txt")]);
    }

    #[test]
    fn typing_filters_the_listing_and_escape_clears_the_filter_first() {
        let setup = Setup::new("filter");
        let mut model = setup.files(false);
        model.key(Setup::press(Key::Char('b')));
        model.key(Setup::press(Key::Char('e')));
        assert!(model.is_filtering());
        assert_eq!(model.navigator().len(), 1);
        assert_eq!(model.key(Setup::press(Key::Escape)), ChooserStep::Redraw);
        assert!(!model.is_filtering());
        assert_eq!(model.navigator().len(), 4);
        assert_eq!(model.key(Setup::press(Key::Escape)), ChooserStep::Cancelled);
    }

    #[test]
    fn entering_a_folder_leaves_filtering() {
        let setup = Setup::new("filter_leaves");
        let mut model = setup.files(false);
        model.key(Setup::press(Key::Char('s')));
        model.key(Setup::press(Key::Enter));
        assert_eq!(model.navigator().path(), setup.dir.path().join("sub"));
        assert!(!model.is_filtering());
    }

    #[test]
    fn enter_after_filtering_to_one_file_picks_it() {
        let setup = Setup::new("filter_pick");
        let mut model = setup.files(false);
        model.key(Setup::press(Key::Char('b')));
        model.key(Setup::press(Key::Char('e')));
        let step = model.key(Setup::press(Key::Enter));
        assert_eq!(Setup::paths(&step), vec![setup.dir.path().join("beta.txt")]);
    }

    #[test]
    fn enter_with_several_matches_and_no_cursor_does_nothing() {
        let setup = Setup::new("filter_ambiguous");
        let mut model = setup.files(false);
        model.key(Setup::press(Key::Char('t')));
        assert!(model.navigator().len() > 1);
        assert_eq!(model.key(Setup::press(Key::Enter)), ChooserStep::Ignored);
    }

    #[test]
    fn backspace_goes_up_and_back_returns() {
        let setup = Setup::new("up");
        let mut model = setup.files(false);
        model.activate_entry(Setup::row(&model, "sub"));
        assert_eq!(model.key(Setup::press(Key::Backspace)), ChooserStep::Moved);
        assert_eq!(model.navigator().path(), setup.dir.path());
    }

    #[test]
    fn going_up_leaves_the_folder_and_reports_a_move() {
        let setup = Setup::new("go_up");
        let mut model = setup.files(false);
        model.activate_entry(Setup::row(&model, "sub"));
        assert_eq!(model.go_up(), ChooserStep::Moved);
        assert_eq!(model.navigator().path(), setup.dir.path());
    }

    #[test]
    fn translate_reports_the_action_a_key_means_without_acting() {
        let setup = Setup::new("translate");
        let model = setup.files(false);
        assert_eq!(
            model.translate(Setup::press(Key::Escape)),
            Some(ChooserAction::Cancel)
        );
        assert_eq!(model.navigator().len(), 4);
    }

    #[test]
    fn the_hidden_toggle_reveals_dotfiles() {
        let setup = Setup::new("hidden");
        let mut model = setup.files(false);
        model.act(ChooserAction::Browse(BrowseAction::ToggleHidden));
        assert_eq!(model.navigator().len(), 5);
    }

    #[test]
    fn a_key_with_no_meaning_changes_nothing() {
        let setup = Setup::new("unbound");
        let mut model = setup.files(false);
        assert_eq!(model.key(Setup::press(Key::Tab)), ChooserStep::Ignored);
    }

    #[test]
    fn the_selection_carries_no_choices_or_filter_yet() {
        let setup = Setup::new("plain_selection");
        let mut model = setup.files(false);
        model.pick(Setup::row(&model, "alpha.txt"), Mods::none());
        match model.accept() {
            ChooserStep::Finished(selection) => {
                assert!(selection.choices.is_empty());
                assert!(selection.current_filter.is_none());
                assert!(!selection.writable);
            }
            other => panic!("expected a selection, got {other:?}"),
        }
    }
}
