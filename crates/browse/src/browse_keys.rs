use crate::browse_action::BrowseAction;
use crate::edit_action::EditAction;
use platform::toplevel::{Key, KeyPress};
use widget::{Direction, Edge, ListCommand};

pub struct BrowseKeys;

impl BrowseKeys {
    #[must_use]
    pub const fn list(press: KeyPress) -> Option<BrowseAction> {
        if press.modifiers.ctrl() {
            return match press.key {
                Key::Char('a' | 'A') => Some(BrowseAction::List(ListCommand::SelectAll)),
                Key::Char('h' | 'H') => Some(BrowseAction::ToggleHidden),
                Key::Char('r' | 'R') => Some(BrowseAction::Refresh),
                Key::Char('l' | 'L') => Some(BrowseAction::BeginFilter),
                _ => None,
            };
        }

        match press.key {
            Key::Enter | Key::Right => Some(BrowseAction::List(ListCommand::Activate)),
            Key::Left | Key::Backspace => Some(BrowseAction::GoUp),
            Key::Escape => Some(BrowseAction::List(ListCommand::ClearSelection)),
            Key::Home => Some(BrowseAction::List(ListCommand::Jump(Edge::Top))),
            Key::End => Some(BrowseAction::List(ListCommand::Jump(Edge::Bottom))),
            Key::Function(5) => Some(BrowseAction::Refresh),
            Key::Char(c) if c != ' ' => Some(BrowseAction::Insert(c)),
            _ => Self::movement(press),
        }
    }

    #[must_use]
    pub const fn filter(press: KeyPress) -> Option<BrowseAction> {
        if press.modifiers.ctrl() {
            return Self::text(press);
        }

        match press.key {
            Key::Enter => Some(BrowseAction::List(ListCommand::Activate)),
            Key::Escape => Some(BrowseAction::CancelFilter),
            Key::Tab => Some(BrowseAction::EndFilter),
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => Self::movement(press),
            _ => Self::text(press),
        }
    }

    #[must_use]
    pub const fn text(press: KeyPress) -> Option<BrowseAction> {
        if press.modifiers.ctrl() {
            return match press.key {
                Key::Char('a' | 'A') => Some(BrowseAction::Edit(EditAction::Home)),
                Key::Char('e' | 'E') => Some(BrowseAction::Edit(EditAction::End)),
                Key::Char('k' | 'K') => Some(BrowseAction::Edit(EditAction::KillToEnd)),
                Key::Char('u' | 'U') => Some(BrowseAction::Edit(EditAction::KillToStart)),
                Key::Char('w' | 'W') => Some(BrowseAction::Edit(EditAction::DeleteWordLeft)),
                Key::Left => Some(BrowseAction::Edit(EditAction::WordLeft)),
                Key::Right => Some(BrowseAction::Edit(EditAction::WordRight)),
                _ => None,
            };
        }

        match press.key {
            Key::Backspace => Some(BrowseAction::Edit(EditAction::Backspace)),
            Key::Delete => Some(BrowseAction::Edit(EditAction::Delete)),
            Key::Left => Some(BrowseAction::Edit(EditAction::Left)),
            Key::Right => Some(BrowseAction::Edit(EditAction::Right)),
            Key::Home => Some(BrowseAction::Edit(EditAction::Home)),
            Key::End => Some(BrowseAction::Edit(EditAction::End)),
            Key::Char(c) => Some(BrowseAction::Insert(c)),
            _ => None,
        }
    }

    const fn movement(press: KeyPress) -> Option<BrowseAction> {
        let shift = press.modifiers.shift();
        match press.key {
            Key::Up if shift => Some(BrowseAction::List(ListCommand::Extend(Direction::Up))),
            Key::Down if shift => Some(BrowseAction::List(ListCommand::Extend(Direction::Down))),
            Key::Up => Some(BrowseAction::List(ListCommand::Move(Direction::Up))),
            Key::Down => Some(BrowseAction::List(ListCommand::Move(Direction::Down))),
            Key::PageUp => Some(BrowseAction::List(ListCommand::Page(Direction::Up))),
            Key::PageDown => Some(BrowseAction::List(ListCommand::Page(Direction::Down))),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BrowseKeys;
    use crate::browse_action::BrowseAction;
    use crate::edit_action::EditAction;
    use platform::toplevel::{Key, KeyPress, ModifierState};
    use widget::{Direction, Edge, ListCommand};

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

    #[test]
    fn arrows_move_and_shift_arrows_extend_on_the_list() {
        assert_eq!(
            BrowseKeys::list(press(Key::Down)),
            Some(BrowseAction::List(ListCommand::Move(Direction::Down)))
        );
        assert_eq!(
            BrowseKeys::list(with_mods(Key::Up, SHIFT)),
            Some(BrowseAction::List(ListCommand::Extend(Direction::Up)))
        );
    }

    #[test]
    fn paging_and_edges_reach_the_list() {
        assert_eq!(
            BrowseKeys::list(press(Key::PageDown)),
            Some(BrowseAction::List(ListCommand::Page(Direction::Down)))
        );
        assert_eq!(
            BrowseKeys::list(press(Key::Home)),
            Some(BrowseAction::List(ListCommand::Jump(Edge::Top)))
        );
        assert_eq!(
            BrowseKeys::list(press(Key::End)),
            Some(BrowseAction::List(ListCommand::Jump(Edge::Bottom)))
        );
    }

    #[test]
    fn enter_and_right_activate_and_left_and_backspace_go_up() {
        let activate = Some(BrowseAction::List(ListCommand::Activate));
        assert_eq!(BrowseKeys::list(press(Key::Enter)), activate);
        assert_eq!(BrowseKeys::list(press(Key::Right)), activate);
        assert_eq!(
            BrowseKeys::list(press(Key::Left)),
            Some(BrowseAction::GoUp)
        );
        assert_eq!(
            BrowseKeys::list(press(Key::Backspace)),
            Some(BrowseAction::GoUp)
        );
    }

    #[test]
    fn a_bare_letter_starts_filtering_but_space_does_not() {
        assert_eq!(
            BrowseKeys::list(press(Key::Char('r'))),
            Some(BrowseAction::Insert('r'))
        );
        assert_eq!(BrowseKeys::list(press(Key::Char(' '))), None);
    }

    #[test]
    fn ctrl_letters_on_the_list_are_commands_and_unknown_ones_are_inert() {
        assert_eq!(
            BrowseKeys::list(with_mods(Key::Char('A'), CTRL)),
            Some(BrowseAction::List(ListCommand::SelectAll))
        );
        assert_eq!(
            BrowseKeys::list(with_mods(Key::Char('h'), CTRL)),
            Some(BrowseAction::ToggleHidden)
        );
        assert_eq!(
            BrowseKeys::list(with_mods(Key::Char('r'), CTRL)),
            Some(BrowseAction::Refresh)
        );
        assert_eq!(
            BrowseKeys::list(with_mods(Key::Char('l'), CTRL)),
            Some(BrowseAction::BeginFilter)
        );
        assert_eq!(BrowseKeys::list(with_mods(Key::Char('z'), CTRL)), None);
    }

    #[test]
    fn the_filter_edits_text_but_still_walks_the_results() {
        assert_eq!(
            BrowseKeys::filter(press(Key::Backspace)),
            Some(BrowseAction::Edit(EditAction::Backspace))
        );
        assert_eq!(
            BrowseKeys::filter(press(Key::Left)),
            Some(BrowseAction::Edit(EditAction::Left))
        );
        assert_eq!(
            BrowseKeys::filter(press(Key::Down)),
            Some(BrowseAction::List(ListCommand::Move(Direction::Down)))
        );
        assert_eq!(
            BrowseKeys::filter(press(Key::Enter)),
            Some(BrowseAction::List(ListCommand::Activate))
        );
        assert_eq!(
            BrowseKeys::filter(press(Key::Char(' '))),
            Some(BrowseAction::Insert(' '))
        );
    }

    #[test]
    fn escape_cancels_the_filter_and_tab_ends_it() {
        assert_eq!(
            BrowseKeys::filter(press(Key::Escape)),
            Some(BrowseAction::CancelFilter)
        );
        assert_eq!(
            BrowseKeys::filter(press(Key::Tab)),
            Some(BrowseAction::EndFilter)
        );
    }

    #[test]
    fn the_filter_has_no_list_commands_on_ctrl_letters() {
        assert_eq!(
            BrowseKeys::filter(with_mods(Key::Char('a'), CTRL)),
            Some(BrowseAction::Edit(EditAction::Home))
        );
        assert_eq!(BrowseKeys::filter(with_mods(Key::Char('h'), CTRL)), None);
    }

    #[test]
    fn a_bare_text_field_gets_readline_motions_and_nothing_from_the_list() {
        for (ch, expected) in [
            ('a', EditAction::Home),
            ('e', EditAction::End),
            ('k', EditAction::KillToEnd),
            ('u', EditAction::KillToStart),
            ('w', EditAction::DeleteWordLeft),
        ] {
            assert_eq!(
                BrowseKeys::text(with_mods(Key::Char(ch), CTRL)),
                Some(BrowseAction::Edit(expected)),
                "ctrl-{ch}"
            );
        }
        assert_eq!(BrowseKeys::text(press(Key::Down)), None);
        assert_eq!(BrowseKeys::text(press(Key::Enter)), None);
        assert_eq!(BrowseKeys::text(press(Key::Escape)), None);
        assert_eq!(
            BrowseKeys::text(with_mods(Key::Left, CTRL)),
            Some(BrowseAction::Edit(EditAction::WordLeft))
        );
    }

    #[test]
    fn delete_edits_text_in_both_text_contexts() {
        let edit = Some(BrowseAction::Edit(EditAction::Delete));
        assert_eq!(BrowseKeys::text(press(Key::Delete)), edit);
        assert_eq!(BrowseKeys::filter(press(Key::Delete)), edit);
        assert_eq!(BrowseKeys::text(with_mods(Key::Delete, SHIFT)), edit);
    }

    #[test]
    fn the_list_does_not_bind_delete() {
        assert_eq!(BrowseKeys::list(press(Key::Delete)), None);
    }

    #[test]
    fn an_unbound_key_means_nothing() {
        assert_eq!(BrowseKeys::list(press(Key::Insert)), None);
        assert_eq!(BrowseKeys::list(press(Key::Menu)), None);
    }
}
