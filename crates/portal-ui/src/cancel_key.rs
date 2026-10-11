use platform::toplevel::{Key, KeyPress};

pub struct CancelKey;

impl CancelKey {
    #[must_use]
    pub fn matches(press: KeyPress) -> bool {
        press.key == Key::Escape && !press.repeat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::ModifierState;

    struct Press;

    impl Press {
        fn of(key: Key, repeat: bool) -> KeyPress {
            KeyPress {
                key,
                modifiers: ModifierState::from_wire(0, 0, 0, 0),
                repeat,
            }
        }
    }

    #[test]
    fn escape_cancels() {
        assert!(CancelKey::matches(Press::of(Key::Escape, false)));
    }

    #[test]
    fn a_repeated_escape_is_ignored() {
        assert!(!CancelKey::matches(Press::of(Key::Escape, true)));
    }

    #[test]
    fn other_keys_do_not_cancel() {
        assert!(!CancelKey::matches(Press::of(Key::Enter, false)));
        assert!(!CancelKey::matches(Press::of(Key::Char('q'), false)));
    }
}
