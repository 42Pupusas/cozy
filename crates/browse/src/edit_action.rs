use widget::TextInput;

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

impl EditAction {
    pub fn apply(self, input: &mut TextInput) -> bool {
        match self {
            Self::Backspace => input.backspace(),
            Self::Delete => input.delete(),
            Self::Left => input.move_left(),
            Self::Right => input.move_right(),
            Self::WordLeft => input.move_word_left(),
            Self::WordRight => input.move_word_right(),
            Self::Home => input.move_home(),
            Self::End => input.move_end(),
            Self::KillToEnd => input.delete_to_end(),
            Self::KillToStart => input.delete_to_start(),
            Self::DeleteWordLeft => input.delete_word_left(),
        }
    }
}
