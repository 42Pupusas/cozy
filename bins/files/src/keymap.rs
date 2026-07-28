//! [`Keymap`] -- what each key press *means* in this browser.
//!
//! # Why the binary owns this
//!
//! `crates/widget` deliberately depends on nothing, so it cannot see
//! `platform::toplevel::Key`; `crates/platform` knows nothing about lists, so
//! it cannot produce a `ListCommand`. The translation between them is
//! *policy* -- does Ctrl-A select every file, or jump to the start of the
//! filter box? does Backspace go up a directory, or delete a character? --
//! and policy belongs to the application, not to either library.
//!
//! Putting it in its own module rather than inline in the window keeps it
//! testable against the real `Key` type without a compositor, which is the
//! only way to catch a binding that silently does nothing.
//!
//! # Modal, because a text field changes what letters mean
//!
//! With the filter box focused, `n` types the letter n. With the list
//! focused, `n` is free to be a shortcut. That is not a detail that can be
//! decided per-key: it is a mode, and modelling it as one ([`Focus`]) keeps
//! every branch in a single readable place instead of scattering
//! `if self.filtering` through a forty-arm match.

use platform::toplevel::{Key, KeyPress};
use widget::{Direction, Edge, ListCommand};

/// Which surface has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The file list. Letters are available as shortcuts.
    #[default]
    List,
    /// The filter box. Letters type.
    Filter,
}

/// What a key press asked the application to do.
///
/// The list commands are passed through as one variant rather than flattened,
/// so the window can hand them straight to the pane without rebuilding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Do something to the list.
    List(ListCommand),
    /// Type a character into the filter box.
    Insert(char),
    /// An edit to the filter box that is not an insertion.
    Edit(EditAction),
    /// Move focus to the filter box and start filtering.
    BeginFilter,
    /// Leave the filter box, keeping the query.
    EndFilter,
    /// Leave the filter box and clear the query.
    CancelFilter,
    /// Navigate to the parent directory.
    GoUp,
    /// Re-read the current directory.
    Refresh,
    /// Show or hide dotfiles.
    ToggleHidden,
    /// Close the window.
    Quit,
}

/// A non-inserting edit in the filter box.
///
/// A separate enum from [`Action`] so the window's filter handling is one
/// exhaustive match over edits, and adding an edit cannot accidentally be
/// forgotten in the list-focus path where it has no meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToEnd,
    KillToStart,
    DeleteWordLeft,
}

/// Translates key presses into [`Action`]s according to the current focus.
///
/// A unit struct with a `focus` field rather than free functions: the
/// translation genuinely depends on state, and threading that state through
/// as an argument at every call site is how it ends up out of sync.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keymap {
    focus: Focus,
}

impl Keymap {
    #[must_use]
    pub const fn new() -> Self {
        Self { focus: Focus::List }
    }

    /// Where the keyboard currently goes.
    ///
    /// Only the tests read this today -- the window asks the narrower
    /// [`Self::is_filtering`] instead -- but the accessor is the honest way
    /// to assert on the mode, and a mode with no way to observe it is a mode
    /// with no tests.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn focus(self) -> Focus {
        self.focus
    }

    /// Whether the filter box is active (and so should draw a caret).
    #[must_use]
    pub const fn is_filtering(self) -> bool {
        matches!(self.focus, Focus::Filter)
    }

    /// Move focus.
    pub const fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    /// Translate one key press, or `None` if it means nothing here.
    #[must_use]
    pub const fn translate(self, press: KeyPress) -> Option<Action> {
        match self.focus {
            Focus::List => Self::in_list(press),
            Focus::Filter => Self::in_filter(press),
        }
    }

    /// Bindings while the list has focus.
    const fn in_list(press: KeyPress) -> Option<Action> {
        let ctrl = press.modifiers.ctrl();
        let shift = press.modifiers.shift();

        // Ctrl combinations first: with Ctrl held, a letter is a command and
        // never a shortcut-letter. Checking the plain letters first would
        // make Ctrl-H toggle hidden files *and* match a bare `h`.
        if ctrl {
            return match press.key {
                Key::Char('a' | 'A') => Some(Action::List(ListCommand::SelectAll)),
                Key::Char('h' | 'H') => Some(Action::ToggleHidden),
                Key::Char('r' | 'R') => Some(Action::Refresh),
                Key::Char('l' | 'L') => Some(Action::BeginFilter),
                Key::Char('q' | 'Q') => Some(Action::Quit),
                _ => None,
            };
        }

        match press.key {
            Key::Up if shift => Some(Action::List(ListCommand::Extend(Direction::Up))),
            Key::Down if shift => Some(Action::List(ListCommand::Extend(Direction::Down))),
            Key::Up => Some(Action::List(ListCommand::Move(Direction::Up))),
            Key::Down => Some(Action::List(ListCommand::Move(Direction::Down))),
            Key::PageUp => Some(Action::List(ListCommand::Page(Direction::Up))),
            Key::PageDown => Some(Action::List(ListCommand::Page(Direction::Down))),
            Key::Home => Some(Action::List(ListCommand::Jump(Edge::Top))),
            Key::End => Some(Action::List(ListCommand::Jump(Edge::Bottom))),
            // Right descends, mirroring Left's ascent -- both are how a
            // keyboard-only user moves through a tree.
            Key::Enter | Key::Right => Some(Action::List(ListCommand::Activate)),
            Key::Left | Key::Backspace => Some(Action::GoUp),
            Key::Escape => Some(Action::List(ListCommand::ClearSelection)),
            Key::Function(5) => Some(Action::Refresh),
            // Typing a printable character opens the filter and types into
            // it, so search-as-you-type needs no prefix key. Space is
            // excluded: it is a selection gesture in every file manager, and
            // a leading space in a filter is invisible and baffling.
            Key::Char(c) if c != ' ' => Some(Action::Insert(c)),
            _ => None,
        }
    }

    /// Bindings while the filter box has focus.
    ///
    /// Navigation keys still reach the *list*: filtering as you type is only
    /// useful if you can arrow down to a result without leaving the box.
    const fn in_filter(press: KeyPress) -> Option<Action> {
        let ctrl = press.modifiers.ctrl();
        let shift = press.modifiers.shift();

        if ctrl {
            return match press.key {
                Key::Char('a' | 'A') => Some(Action::Edit(EditAction::Home)),
                Key::Char('e' | 'E') => Some(Action::Edit(EditAction::End)),
                Key::Char('k' | 'K') => Some(Action::Edit(EditAction::KillToEnd)),
                Key::Char('u' | 'U') => Some(Action::Edit(EditAction::KillToStart)),
                Key::Char('w' | 'W') => Some(Action::Edit(EditAction::DeleteWordLeft)),
                Key::Char('q' | 'Q') => Some(Action::Quit),
                Key::Left => Some(Action::Edit(EditAction::WordLeft)),
                Key::Right => Some(Action::Edit(EditAction::WordRight)),
                _ => None,
            };
        }

        match press.key {
            // The list keeps the vertical keys so results can be walked
            // without dropping out of the box.
            Key::Up if shift => Some(Action::List(ListCommand::Extend(Direction::Up))),
            Key::Down if shift => Some(Action::List(ListCommand::Extend(Direction::Down))),
            Key::Up => Some(Action::List(ListCommand::Move(Direction::Up))),
            Key::Down => Some(Action::List(ListCommand::Move(Direction::Down))),
            Key::PageUp => Some(Action::List(ListCommand::Page(Direction::Up))),
            Key::PageDown => Some(Action::List(ListCommand::Page(Direction::Down))),
            // Enter commits: act on the highlighted result and leave the box.
            Key::Enter => Some(Action::List(ListCommand::Activate)),
            Key::Escape => Some(Action::CancelFilter),
            Key::Tab => Some(Action::EndFilter),
            Key::Backspace => Some(Action::Edit(EditAction::Backspace)),
            Key::Delete => Some(Action::Edit(EditAction::Delete)),
            Key::Left => Some(Action::Edit(EditAction::Left)),
            Key::Right => Some(Action::Edit(EditAction::Right)),
            Key::Home => Some(Action::Edit(EditAction::Home)),
            Key::End => Some(Action::Edit(EditAction::End)),
            Key::Char(c) => Some(Action::Insert(c)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, EditAction, Focus, Keymap};
    use platform::toplevel::{Key, KeyPress, ModifierState};
    use widget::{Direction, Edge, ListCommand};

    /// The XKB mask bit positions, as `ModifierState` decodes them.
    const SHIFT: u32 = 1 << 0;
    const CTRL: u32 = 1 << 2;

    fn press(key: Key) -> KeyPress {
        KeyPress {
            key,
            modifiers: ModifierState::default(),
            repeat: false,
        }
    }

    fn with_mods(key: Key, mask: u32) -> KeyPress {
        KeyPress {
            key,
            modifiers: ModifierState::from_wire(mask, 0, 0, 0),
            repeat: false,
        }
    }

    fn list_map() -> Keymap {
        Keymap::new()
    }

    fn filter_map() -> Keymap {
        let mut map = Keymap::new();
        map.set_focus(Focus::Filter);
        map
    }

    #[test]
    fn a_fresh_keymap_starts_on_the_list() {
        let map = Keymap::new();
        assert_eq!(map.focus(), Focus::List);
        assert!(!map.is_filtering());
    }

    #[test]
    fn arrows_move_the_cursor() {
        let map = list_map();
        assert_eq!(
            map.translate(press(Key::Down)),
            Some(Action::List(ListCommand::Move(Direction::Down)))
        );
        assert_eq!(
            map.translate(press(Key::Up)),
            Some(Action::List(ListCommand::Move(Direction::Up)))
        );
    }

    #[test]
    fn shift_arrows_extend_the_selection() {
        let map = list_map();
        assert_eq!(
            map.translate(with_mods(Key::Down, SHIFT)),
            Some(Action::List(ListCommand::Extend(Direction::Down)))
        );
    }

    #[test]
    fn home_and_end_jump_to_the_edges() {
        let map = list_map();
        assert_eq!(
            map.translate(press(Key::Home)),
            Some(Action::List(ListCommand::Jump(Edge::Top)))
        );
        assert_eq!(
            map.translate(press(Key::End)),
            Some(Action::List(ListCommand::Jump(Edge::Bottom)))
        );
    }

    #[test]
    fn enter_and_right_both_descend() {
        let map = list_map();
        let activate = Some(Action::List(ListCommand::Activate));
        assert_eq!(map.translate(press(Key::Enter)), activate);
        assert_eq!(map.translate(press(Key::Right)), activate);
    }

    #[test]
    fn left_and_backspace_both_go_up() {
        let map = list_map();
        assert_eq!(map.translate(press(Key::Left)), Some(Action::GoUp));
        assert_eq!(map.translate(press(Key::Backspace)), Some(Action::GoUp));
    }

    /// The bug this ordering exists to prevent: with Ctrl held, `h` must be
    /// the toggle-hidden command and must *not* also be typed into a filter.
    #[test]
    fn ctrl_letters_are_commands_not_text() {
        let map = list_map();
        assert_eq!(
            map.translate(with_mods(Key::Char('h'), CTRL)),
            Some(Action::ToggleHidden)
        );
        assert_eq!(
            map.translate(with_mods(Key::Char('a'), CTRL)),
            Some(Action::List(ListCommand::SelectAll))
        );
        assert_eq!(map.translate(with_mods(Key::Char('r'), CTRL)), Some(Action::Refresh));
        assert_eq!(map.translate(with_mods(Key::Char('q'), CTRL)), Some(Action::Quit));
    }

    /// Caps Lock or Shift must not silently disable every shortcut.
    #[test]
    fn ctrl_shortcuts_accept_an_uppercase_letter() {
        let map = list_map();
        assert_eq!(
            map.translate(with_mods(Key::Char('A'), CTRL)),
            Some(Action::List(ListCommand::SelectAll))
        );
    }

    /// Search-as-you-type: a bare letter on the list opens the filter.
    #[test]
    fn a_bare_letter_starts_typing_into_the_filter() {
        let map = list_map();
        assert_eq!(map.translate(press(Key::Char('r'))), Some(Action::Insert('r')));
    }

    /// Space is a selection gesture, and a filter starting with an invisible
    /// space would look like the browser had simply stopped matching.
    #[test]
    fn space_does_not_start_a_filter() {
        let map = list_map();
        assert_eq!(map.translate(press(Key::Char(' '))), None);
    }

    #[test]
    fn an_unbound_key_means_nothing() {
        let map = list_map();
        assert_eq!(map.translate(press(Key::Insert)), None);
        assert_eq!(map.translate(press(Key::Menu)), None);
    }

    // -- filter focus -----------------------------------------------------

    #[test]
    fn letters_type_while_filtering() {
        let map = filter_map();
        assert!(map.is_filtering());
        assert_eq!(map.translate(press(Key::Char('x'))), Some(Action::Insert('x')));
    }

    /// A space is perfectly valid *inside* a query -- it is only the leading
    /// one, typed on the list, that is refused.
    #[test]
    fn a_space_types_normally_once_filtering_has_started() {
        let map = filter_map();
        assert_eq!(map.translate(press(Key::Char(' '))), Some(Action::Insert(' ')));
    }

    #[test]
    fn editing_keys_reach_the_text_field() {
        let map = filter_map();
        assert_eq!(
            map.translate(press(Key::Backspace)),
            Some(Action::Edit(EditAction::Backspace))
        );
        assert_eq!(
            map.translate(press(Key::Left)),
            Some(Action::Edit(EditAction::Left))
        );
        assert_eq!(
            map.translate(press(Key::Home)),
            Some(Action::Edit(EditAction::Home))
        );
    }

    /// The reason navigation is duplicated into the filter map: filtering as
    /// you type is only useful if you can walk the results without leaving
    /// the box.
    #[test]
    fn vertical_navigation_still_reaches_the_list_while_filtering() {
        let map = filter_map();
        assert_eq!(
            map.translate(press(Key::Down)),
            Some(Action::List(ListCommand::Move(Direction::Down)))
        );
        assert_eq!(
            map.translate(press(Key::PageDown)),
            Some(Action::List(ListCommand::Page(Direction::Down)))
        );
        assert_eq!(
            map.translate(press(Key::Enter)),
            Some(Action::List(ListCommand::Activate))
        );
    }

    /// Backspace must edit the query, not navigate away -- losing a typed
    /// query by pressing Backspace one time too many is infuriating.
    #[test]
    fn backspace_edits_rather_than_navigating_while_filtering() {
        assert_eq!(
            filter_map().translate(press(Key::Backspace)),
            Some(Action::Edit(EditAction::Backspace))
        );
        assert_eq!(list_map().translate(press(Key::Backspace)), Some(Action::GoUp));
    }

    /// Similarly, Left must move the caret rather than leaving the directory.
    #[test]
    fn left_moves_the_caret_rather_than_going_up_while_filtering() {
        assert_eq!(
            filter_map().translate(press(Key::Left)),
            Some(Action::Edit(EditAction::Left))
        );
        assert_eq!(list_map().translate(press(Key::Left)), Some(Action::GoUp));
    }

    #[test]
    fn escape_cancels_and_tab_commits() {
        let map = filter_map();
        assert_eq!(map.translate(press(Key::Escape)), Some(Action::CancelFilter));
        assert_eq!(map.translate(press(Key::Tab)), Some(Action::EndFilter));
    }

    #[test]
    fn readline_editing_bindings_work_in_the_filter() {
        let map = filter_map();
        for (ch, expected) in [
            ('a', EditAction::Home),
            ('e', EditAction::End),
            ('k', EditAction::KillToEnd),
            ('u', EditAction::KillToStart),
            ('w', EditAction::DeleteWordLeft),
        ] {
            assert_eq!(
                map.translate(with_mods(Key::Char(ch), CTRL)),
                Some(Action::Edit(expected)),
                "ctrl-{ch}"
            );
        }
    }

    /// Ctrl-A means two different things in the two modes, which is the
    /// entire justification for the mode existing.
    #[test]
    fn ctrl_a_is_select_all_on_the_list_but_line_start_in_the_filter() {
        assert_eq!(
            list_map().translate(with_mods(Key::Char('a'), CTRL)),
            Some(Action::List(ListCommand::SelectAll))
        );
        assert_eq!(
            filter_map().translate(with_mods(Key::Char('a'), CTRL)),
            Some(Action::Edit(EditAction::Home))
        );
    }

    /// Quit must work from either mode, or a filtering user is stuck.
    #[test]
    fn quit_works_in_both_modes() {
        assert_eq!(
            list_map().translate(with_mods(Key::Char('q'), CTRL)),
            Some(Action::Quit)
        );
        assert_eq!(
            filter_map().translate(with_mods(Key::Char('q'), CTRL)),
            Some(Action::Quit)
        );
    }
}
