//! Keysym constants and the decode from "XKB keysym" to "named key".
//!
//! # Why this is its own module
//!
//! These constants were being redeclared at every call site that needed
//! them: `bins/launcher/src/launcher_window.rs` has its own private block of
//! `XKB_KEY_*` consts, and `crates/xkb`'s tests declare a third set. Each
//! copy is a chance for a transposed hex digit that silently makes a key
//! stop working, and nothing cross-checks them.
//!
//! X11 keysyms are a frozen, published registry -- they are never going to
//! change -- so the values are stated once here and the mapping to a
//! semantic [`Key`] happens in one tested place.
//!
//! # Why not put this in `crates/xkb`
//!
//! That crate is a thin, honest binding to `libxkbcommon`: it answers
//! "what keysym does this keycode produce", which is a question about the
//! *keymap*. Which keysyms a GUI considers "navigation" is a question about
//! the *toolkit*. Keeping the opinion out of the binding means `xkb` stays
//! reusable by anything that wants raw keysyms.

/// The X11 keysyms a GUI branches on.
///
/// Named rather than spelled as bare hex at each use, and grouped so the
/// blocks (`0xff00` functional, `0xffbe` function keys) stay visible.
pub mod sym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const DELETE: u32 = 0xffff;

    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;

    pub const INSERT: u32 = 0xff63;
    pub const MENU: u32 = 0xff67;

    /// The numeric keypad's Enter, which is a *different* keysym from
    /// `RETURN` and is missed by anyone who only checks the main one.
    pub const KP_ENTER: u32 = 0xff8d;

    pub const F1: u32 = 0xffbe;
    pub const F12: u32 = 0xffc9;
}

/// A key press reduced to what a widget actually dispatches on.
///
/// Text and named keys are deliberately one enum: a key event is exactly one
/// of "this typed a character" or "this asked for an action", never both,
/// and modelling it as a pair of `Option`s pushes that either/or into every
/// caller's match arms.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// The key produced text. Always a real character -- control codepoints
    /// are filtered out before this is built, so a caller can insert it into
    /// a buffer without re-checking.
    Char(char),
    Backspace,
    Delete,
    Enter,
    Escape,
    Tab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Menu,
    /// Function key, 1-12.
    Function(u8),
}

impl Key {
    /// Decode a keysym plus the Unicode codepoint XKB says it produces.
    ///
    /// Named keys are checked **first**, because several of them also carry
    /// a legacy control codepoint: Return reports U+000D, Tab U+0009,
    /// Backspace U+0008, Escape U+001B. Trusting the codepoint first would
    /// turn Enter into a stray carriage return inserted mid-buffer.
    ///
    /// Returns `None` for keys with no meaning here -- bare modifiers, dead
    /// keys mid-compose, unmapped hardware -- so callers do not need to
    /// filter noise out of their own event stream.
    #[must_use]
    pub const fn decode(keysym: u32, utf32: u32) -> Option<Self> {
        if let Some(named) = Self::named(keysym) {
            return Some(named);
        }
        Self::text(utf32)
    }

    /// The named-key half of [`Self::decode`].
    #[must_use]
    pub const fn named(keysym: u32) -> Option<Self> {
        // Function keys are contiguous, so they are a range rather than
        // twelve arms.
        if keysym >= sym::F1 && keysym <= sym::F12 {
            // Cannot exceed 12 given the bounds above, so the cast is exact.
            #[allow(clippy::cast_possible_truncation)]
            return Some(Self::Function((keysym - sym::F1) as u8 + 1));
        }
        match keysym {
            sym::BACKSPACE => Some(Self::Backspace),
            sym::DELETE => Some(Self::Delete),
            sym::RETURN | sym::KP_ENTER => Some(Self::Enter),
            sym::ESCAPE => Some(Self::Escape),
            sym::TAB => Some(Self::Tab),
            sym::UP => Some(Self::Up),
            sym::DOWN => Some(Self::Down),
            sym::LEFT => Some(Self::Left),
            sym::RIGHT => Some(Self::Right),
            sym::HOME => Some(Self::Home),
            sym::END => Some(Self::End),
            sym::PAGE_UP => Some(Self::PageUp),
            sym::PAGE_DOWN => Some(Self::PageDown),
            sym::INSERT => Some(Self::Insert),
            sym::MENU => Some(Self::Menu),
            _ => None,
        }
    }

    /// The text half of [`Self::decode`].
    ///
    /// Control codepoints are rejected: XKB reports them for keys that are
    /// better described by their name, and inserting U+0000..U+001F into a
    /// text field produces invisible corruption rather than a visible error.
    #[must_use]
    pub const fn text(utf32: u32) -> Option<Self> {
        let Some(ch) = char::from_u32(utf32) else {
            return None;
        };
        if (ch as u32) < 0x20 || ch as u32 == 0x7f {
            return None;
        }
        Some(Self::Char(ch))
    }

    /// Whether this key inserts text, as opposed to requesting an action.
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Char(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_letter_decodes_as_text() {
        assert_eq!(Key::decode(0x61, u32::from('a')), Some(Key::Char('a')));
    }

    #[test]
    fn navigation_keys_decode_by_name() {
        assert_eq!(Key::decode(sym::UP, 0), Some(Key::Up));
        assert_eq!(Key::decode(sym::DOWN, 0), Some(Key::Down));
        assert_eq!(Key::decode(sym::LEFT, 0), Some(Key::Left));
        assert_eq!(Key::decode(sym::RIGHT, 0), Some(Key::Right));
        assert_eq!(Key::decode(sym::HOME, 0), Some(Key::Home));
        assert_eq!(Key::decode(sym::END, 0), Some(Key::End));
        assert_eq!(Key::decode(sym::PAGE_UP, 0), Some(Key::PageUp));
        assert_eq!(Key::decode(sym::PAGE_DOWN, 0), Some(Key::PageDown));
    }

    /// The bug this module exists to prevent. Return/Tab/Backspace/Escape
    /// all carry a legacy control codepoint; if the text branch were checked
    /// first, pressing Enter would insert a carriage return into a filename.
    #[test]
    fn keys_carrying_a_legacy_control_codepoint_decode_as_named_not_text() {
        assert_eq!(Key::decode(sym::RETURN, 0x0d), Some(Key::Enter));
        assert_eq!(Key::decode(sym::TAB, 0x09), Some(Key::Tab));
        assert_eq!(Key::decode(sym::BACKSPACE, 0x08), Some(Key::Backspace));
        assert_eq!(Key::decode(sym::ESCAPE, 0x1b), Some(Key::Escape));
    }

    #[test]
    fn the_keypad_enter_is_also_enter() {
        assert_eq!(Key::decode(sym::KP_ENTER, 0), Some(Key::Enter));
    }

    #[test]
    fn function_keys_decode_across_the_whole_range() {
        assert_eq!(Key::decode(sym::F1, 0), Some(Key::Function(1)));
        assert_eq!(Key::decode(sym::F12, 0), Some(Key::Function(12)));
        assert_eq!(Key::decode(sym::F1 + 4, 0), Some(Key::Function(5)));
    }

    #[test]
    fn a_keysym_just_past_the_function_block_is_not_a_function_key() {
        assert_eq!(Key::named(sym::F12 + 1), None);
    }

    #[test]
    fn a_bare_modifier_produces_nothing() {
        // Shift_L: a real keysym, but no symbol and no text.
        assert_eq!(Key::decode(0xffe1, 0), None);
    }

    #[test]
    fn control_codepoints_are_never_inserted_as_text() {
        for cp in 0..0x20 {
            assert_eq!(Key::text(cp), None, "control {cp:#x} leaked through");
        }
        assert_eq!(Key::text(0x7f), None);
    }

    #[test]
    fn non_ascii_text_survives() {
        assert_eq!(Key::decode(0, u32::from('é')), Some(Key::Char('é')));
        assert_eq!(Key::decode(0, 0x1f600), Some(Key::Char('😀')));
    }

    #[test]
    fn a_surrogate_codepoint_is_rejected_rather_than_panicking() {
        assert_eq!(Key::text(0xd800), None);
    }

    #[test]
    fn text_and_action_keys_are_distinguishable() {
        assert!(Key::Char('a').is_text());
        assert!(!Key::Enter.is_text());
    }
}
