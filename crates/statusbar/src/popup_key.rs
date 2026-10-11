//! The keys an open panel reacts to.

use platform::toplevel::keysym::Key as DecodedKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelKey {
    Char(char),
    Backspace,
    Enter,
    Esc,
    Tab,
    Up,
    Down,
    Left,
    Right,
}

impl PanelKey {
    /// `None` for keys no panel acts on (F-keys, Insert, ...).
    #[must_use]
    pub const fn from_key(key: Option<DecodedKey>) -> Option<Self> {
        match key {
            Some(DecodedKey::Char(c)) => Some(Self::Char(c)),
            Some(DecodedKey::Backspace) => Some(Self::Backspace),
            Some(DecodedKey::Enter) => Some(Self::Enter),
            Some(DecodedKey::Escape) => Some(Self::Esc),
            Some(DecodedKey::Tab) => Some(Self::Tab),
            Some(DecodedKey::Up) => Some(Self::Up),
            Some(DecodedKey::Down) => Some(Self::Down),
            Some(DecodedKey::Left) => Some(Self::Left),
            Some(DecodedKey::Right) => Some(Self::Right),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::toplevel::keysym::sym;

    #[test]
    fn text_and_navigation_narrow_to_panel_keys() {
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(0x61, u32::from('a'))),
            Some(PanelKey::Char('a'))
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::UP, 0)),
            Some(PanelKey::Up)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::ESCAPE, 0x1b)),
            Some(PanelKey::Esc)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::TAB, 0x09)),
            Some(PanelKey::Tab)
        );
    }

    #[test]
    fn keys_carrying_a_control_codepoint_stay_actions_not_text() {
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::RETURN, 0x0d)),
            Some(PanelKey::Enter)
        );
        assert_eq!(
            PanelKey::from_key(DecodedKey::decode(sym::BACKSPACE, 0x08)),
            Some(PanelKey::Backspace)
        );
    }

    #[test]
    fn irrelevant_keys_are_ignored() {
        assert_eq!(PanelKey::from_key(DecodedKey::decode(sym::F1, 0)), None);
        assert_eq!(PanelKey::from_key(None), None);
    }
}
