use browse::{BrowseAction, BrowseKeys};
use platform::toplevel::{Key, KeyPress};

use crate::chooser_action::ChooserAction;

pub struct ChooserKeys;

impl ChooserKeys {
    #[must_use]
    pub const fn translate(filtering: bool, press: KeyPress) -> Option<ChooserAction> {
        if press.modifiers.ctrl() && matches!(press.key, Key::Enter) {
            return Some(ChooserAction::Accept);
        }
        if filtering {
            return Self::browse(BrowseKeys::filter(press));
        }
        if matches!(press.key, Key::Escape) {
            return Some(ChooserAction::Cancel);
        }
        Self::browse(BrowseKeys::list(press))
    }

    const fn browse(action: Option<BrowseAction>) -> Option<ChooserAction> {
        match action {
            Some(action) => Some(ChooserAction::Browse(action)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::ModifierState;
    use widget::{Direction, ListCommand};

    const CTRL: u32 = 1 << 2;

    struct Press;

    impl Press {
        fn of(key: Key) -> KeyPress {
            Self::with(key, 0)
        }

        fn with(key: Key, mask: u32) -> KeyPress {
            KeyPress {
                key,
                modifiers: ModifierState::from_wire(mask, 0, 0, 0),
                repeat: false,
            }
        }
    }

    #[test]
    fn control_enter_accepts() {
        assert_eq!(
            ChooserKeys::translate(false, Press::with(Key::Enter, CTRL)),
            Some(ChooserAction::Accept)
        );
        assert_eq!(
            ChooserKeys::translate(true, Press::with(Key::Enter, CTRL)),
            Some(ChooserAction::Accept)
        );
    }

    #[test]
    fn escape_cancels_the_dialog_from_the_list() {
        assert_eq!(
            ChooserKeys::translate(false, Press::of(Key::Escape)),
            Some(ChooserAction::Cancel)
        );
    }

    #[test]
    fn escape_cancels_the_filter_while_filtering() {
        assert_eq!(
            ChooserKeys::translate(true, Press::of(Key::Escape)),
            Some(ChooserAction::Browse(BrowseAction::CancelFilter))
        );
    }

    #[test]
    fn enter_activates_the_cursor_row() {
        assert_eq!(
            ChooserKeys::translate(false, Press::of(Key::Enter)),
            Some(ChooserAction::Browse(BrowseAction::List(
                ListCommand::Activate
            )))
        );
    }

    #[test]
    fn arrows_move_the_cursor() {
        assert_eq!(
            ChooserKeys::translate(false, Press::of(Key::Down)),
            Some(ChooserAction::Browse(BrowseAction::List(
                ListCommand::Move(Direction::Down)
            )))
        );
    }

    #[test]
    fn a_letter_starts_the_filter_on_the_list_and_types_into_it() {
        let expected = Some(ChooserAction::Browse(BrowseAction::Insert('q')));
        assert_eq!(
            ChooserKeys::translate(false, Press::of(Key::Char('q'))),
            expected
        );
        assert_eq!(
            ChooserKeys::translate(true, Press::of(Key::Char('q'))),
            expected
        );
    }
}
