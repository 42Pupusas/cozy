//! Keysym constants and the decode from "XKB keysym" to "named key".
//!
//! # Why this is its own module
//!
//! These constants were being redeclared at every call site that needed
//! them. This module is now the single source for the values, and the
//! former copies read from it:
//!
//! * `bins/launcher/src/launcher_window.rs` had a private block of six
//!   `XKB_KEY_*` consts plus its own named-key match -- now deleted in
//!   favour of [`Key::decode`].
//! * [`crate::keymap`]'s `XkbKeysym` still exists, because it decodes to a
//!   *different* target ([`crate::protocol::Key`], which is physical and
//!   includes modifiers, for the terminal) -- but its numbers come from
//!   [`sym`] rather than a second set of literals.
//!
//! The one remaining copy is deliberate: `crates/xkb`'s tests declare two
//! constants locally because that crate has **no path dependencies at all**
//! by design (it speaks only libxkbcommon's C ABI), and `platform` depends
//! on *it*. Importing this module there would invert the dependency to save
//! two lines in a test.
//!
//! X11 keysyms are a frozen, published registry -- they are never going to
//! change -- so the values are stated once here and the mapping to a
//! semantic [`Key`] happens in one tested place.
//!
//! # What guards this
//!
//! Centralising the numbers removes the "two tables disagree" failure but
//! creates a subtler one: a single wrong constant is now wrong *everywhere
//! at once*, so decoders that read it stay perfectly consistent with each
//! other while all being wrong. A cross-check between decoders cannot see
//! that -- verified by experiment, not assumed. `every_constant_matches_
//! the_x11_registry` below pins each value to its literal from
//! `X11/keysymdef.h`, which is the actual oracle.
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

    /// The function-key block, which is contiguous.
    ///
    /// Callers needing an individual key say `F1 + n` rather than declaring
    /// twelve constants -- twelve more chances to transpose a hex digit,
    /// which is the failure this module exists to prevent.
    pub const F1: u32 = 0xffbe;
    pub const F12: u32 = 0xffc9;

    /// The modifier and lock cluster.
    ///
    /// [`super::Key`] has no variants for these -- a widget never acts on a
    /// bare Shift -- but the terminal's physical keymap does need to name
    /// them, so the values live here with the rest of the registry instead
    /// of in a second table.
    pub const SHIFT_L: u32 = 0xffe1;
    pub const SHIFT_R: u32 = 0xffe2;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const CONTROL_R: u32 = 0xffe4;
    pub const CAPS_LOCK: u32 = 0xffe5;
    pub const NUM_LOCK: u32 = 0xff7f;
    pub const SCROLL_LOCK: u32 = 0xff14;
    pub const ALT_L: u32 = 0xffe9;
    pub const ALT_R: u32 = 0xffea;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SUPER_R: u32 = 0xffec;
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
        // Ctrl changes XKB's UTF-32 result for a letter into a C0 control
        // character (Ctrl-D becomes U+0004). That must not erase the letter:
        // higher layers still need the logical `d` to match shortcuts. The
        // keysym retains that unmodified symbol, so use it only when the text
        // result cannot be inserted.
        match Self::text(utf32) {
            Some(key) => Some(key),
            None => Self::text_from_keysym(keysym),
        }
    }

    /// Recover printable text from an X11 keysym when modifiers made XKB's
    /// UTF-32 result a control character.
    #[must_use]
    const fn text_from_keysym(keysym: u32) -> Option<Self> {
        // Latin-1 keysyms are their Unicode code points. Other Unicode
        // keysyms carry the scalar after the X11 `0x0100_0000` marker.
        if keysym <= 0xff {
            Self::text(keysym)
        } else if keysym & 0xff00_0000 == 0x0100_0000 {
            Self::text(keysym & 0x00ff_ffff)
        } else {
            None
        }
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

    /// Pins every constant to its literal value from the X11 keysym
    /// registry (`X11/keysymdef.h`).
    ///
    /// This looks tautological and is not. Now that `keymap.rs` reads these
    /// same constants, a typo here no longer causes a *disagreement*
    /// between the two decoders -- it moves both in lockstep, so they stay
    /// consistent while both being wrong, and a cross-check between them
    /// passes happily. (Confirmed by experiment: corrupting `PAGE_UP` left
    /// the cross-check in `keymap.rs` green.)
    ///
    /// Centralising the values traded "two tables disagree" for "one table
    /// is silently wrong", and this is what covers the replacement risk.
    /// The registry is frozen, so these literals are a permanent oracle.
    #[test]
    fn every_constant_matches_the_x11_registry() {
        assert_eq!(sym::BACKSPACE, 0xff08);
        assert_eq!(sym::TAB, 0xff09);
        assert_eq!(sym::RETURN, 0xff0d);
        assert_eq!(sym::ESCAPE, 0xff1b);
        assert_eq!(sym::DELETE, 0xffff);

        assert_eq!(sym::HOME, 0xff50);
        assert_eq!(sym::LEFT, 0xff51);
        assert_eq!(sym::UP, 0xff52);
        assert_eq!(sym::RIGHT, 0xff53);
        assert_eq!(sym::DOWN, 0xff54);
        assert_eq!(sym::PAGE_UP, 0xff55);
        assert_eq!(sym::PAGE_DOWN, 0xff56);
        assert_eq!(sym::END, 0xff57);

        assert_eq!(sym::INSERT, 0xff63);
        assert_eq!(sym::MENU, 0xff67);
        assert_eq!(sym::KP_ENTER, 0xff8d);

        assert_eq!(sym::F1, 0xffbe);
        assert_eq!(sym::F12, 0xffc9);

        assert_eq!(sym::SHIFT_L, 0xffe1);
        assert_eq!(sym::SHIFT_R, 0xffe2);
        assert_eq!(sym::CONTROL_L, 0xffe3);
        assert_eq!(sym::CONTROL_R, 0xffe4);
        assert_eq!(sym::CAPS_LOCK, 0xffe5);
        assert_eq!(sym::NUM_LOCK, 0xff7f);
        assert_eq!(sym::SCROLL_LOCK, 0xff14);
        assert_eq!(sym::ALT_L, 0xffe9);
        assert_eq!(sym::ALT_R, 0xffea);
        assert_eq!(sym::SUPER_L, 0xffeb);
        assert_eq!(sym::SUPER_R, 0xffec);
    }

    /// No two keys may share a value -- a copy-paste slip that gave two
    /// constants the same number would make one key silently shadow the
    /// other, and every equality assertion above would still pass.
    #[test]
    fn no_two_constants_collide() {
        let all = [
            ("BACKSPACE", sym::BACKSPACE),
            ("TAB", sym::TAB),
            ("RETURN", sym::RETURN),
            ("ESCAPE", sym::ESCAPE),
            ("DELETE", sym::DELETE),
            ("HOME", sym::HOME),
            ("LEFT", sym::LEFT),
            ("UP", sym::UP),
            ("RIGHT", sym::RIGHT),
            ("DOWN", sym::DOWN),
            ("PAGE_UP", sym::PAGE_UP),
            ("PAGE_DOWN", sym::PAGE_DOWN),
            ("END", sym::END),
            ("INSERT", sym::INSERT),
            ("MENU", sym::MENU),
            ("KP_ENTER", sym::KP_ENTER),
            ("F1", sym::F1),
            ("F12", sym::F12),
            ("SHIFT_L", sym::SHIFT_L),
            ("SHIFT_R", sym::SHIFT_R),
            ("CONTROL_L", sym::CONTROL_L),
            ("CONTROL_R", sym::CONTROL_R),
            ("CAPS_LOCK", sym::CAPS_LOCK),
            ("NUM_LOCK", sym::NUM_LOCK),
            ("SCROLL_LOCK", sym::SCROLL_LOCK),
            ("ALT_L", sym::ALT_L),
            ("ALT_R", sym::ALT_R),
            ("SUPER_L", sym::SUPER_L),
            ("SUPER_R", sym::SUPER_R),
        ];
        for (i, (name_a, a)) in all.iter().enumerate() {
            for (name_b, b) in &all[i + 1..] {
                assert_ne!(a, b, "{name_a} and {name_b} share a keysym value");
            }
        }
    }

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
    fn a_ctrl_modified_letter_keeps_its_logical_symbol() {
        // XKB reports U+0004 as the UTF-32 value for Ctrl-D, but its keysym
        // remains `d`. Without this fallback Ctrl-letter shortcuts never
        // reach application keymaps.
        assert_eq!(Key::decode(u32::from('d'), 0x04), Some(Key::Char('d')));
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
