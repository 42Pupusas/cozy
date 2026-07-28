//! Keymap translation: evdev scancodes → logical key symbols.
//!
//! ## Two paths
//!
//! 1. **Real XKB** (`crate::xkb::Translator`, built once per window from the
//!    compositor's `wl_keyboard.keymap` blob): resolves `(evdev keycode,
//!    live modifier/layout state)` → the actual keysym the compositor's
//!    keymap says that key produces right now — remaps included (e.g.
//!    niri's `input.keyboard.xkb` swapping Esc and `CapsLock`). This is the
//!    path used whenever `libxkbcommon` loaded and the keymap compiled.
//!    [`XkbKeysym::from_raw`] + [`XkbKeysym::to_key`] turn the resulting
//!    keysym into our [`Key`] enum.
//!
//! 2. **Physical-only fallback** ([`PhysicalKey::to_logical`]): used only
//!    when XKB is unavailable (library missing, keymap fd never arrived
//!    yet, compile failure). Sufficient for US-QWERTY, movement keys, and
//!    modifier-aware action keys, but blind to layout remaps, non-US
//!    layouts, dead keys, and IME composition.
//!
//! Both paths ultimately produce the same [`Key`] enum, so callers
//! (`crate::window::TermWindow::resolve_logical_key`) don't need to care
//! which one resolved a given press.

use crate::protocol::Key;
use crate::toplevel::keysym::sym;

/// Extends [`Key`] with physical→logical keymap resolution, as a method
/// instead of a free `physical_to_logical(key)` function.
pub trait PhysicalKey {
    /// Map this physical evdev keycode to its logical keysym via the
    /// no-XKB fallback path (see module docs).
    #[must_use]
    fn to_logical(self) -> Self;
}

impl PhysicalKey for Key {
    fn to_logical(self) -> Self {
        // Physical-only fallback: no layout remap applied. See module docs.
        self
    }
}

/// The XKB keysyms (`xkbcommon/xkbcommon-keysyms.h`) we translate into a
/// [`Key`] variant that has no direct Latin-1/Unicode mapping — navigation,
/// editing, function keys, and the modifier/lock cluster. Printable
/// characters don't need a table: XKB keysyms for ASCII and Latin-1 equal
/// their Unicode codepoint directly (`xkb_keysym_to_utf32` handles the rest
/// of Unicode, including non-Latin1 keysyms, without needing named
/// constants here).
///
/// Real, closed C-enum set — kept as a `#[repr(u32)] enum` rather than a
/// loose `const` group, matching the rest of this workspace's FFI-adjacent
/// code (e.g. `Key`/`SeatCapability` just above/below in `protocol.rs`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum XkbKeysym {
    BackSpace = sym::BACKSPACE,
    Tab = sym::TAB,
    Return = sym::RETURN,
    Escape = sym::ESCAPE,
    Delete = sym::DELETE,
    Insert = sym::INSERT,
    Home = sym::HOME,
    End = sym::END,
    PageUp = sym::PAGE_UP,
    PageDown = sym::PAGE_DOWN,
    Left = sym::LEFT,
    Up = sym::UP,
    Right = sym::RIGHT,
    Down = sym::DOWN,
    F1 = sym::F1,
    F2 = sym::F1 + 1,
    F3 = sym::F1 + 2,
    F4 = sym::F1 + 3,
    F5 = sym::F1 + 4,
    F6 = sym::F1 + 5,
    F7 = sym::F1 + 6,
    F8 = sym::F1 + 7,
    F9 = sym::F1 + 8,
    F10 = sym::F1 + 9,
    F11 = sym::F1 + 10,
    F12 = sym::F12,
    ShiftL = sym::SHIFT_L,
    ShiftR = sym::SHIFT_R,
    ControlL = sym::CONTROL_L,
    ControlR = sym::CONTROL_R,
    CapsLock = sym::CAPS_LOCK,
    NumLock = sym::NUM_LOCK,
    ScrollLock = sym::SCROLL_LOCK,
    AltL = sym::ALT_L,
    AltR = sym::ALT_R,
    SuperL = sym::SUPER_L,
    SuperR = sym::SUPER_R,
    Menu = sym::MENU,
    KpEnter = sym::KP_ENTER,
}

impl XkbKeysym {
    /// (raw XKB keysym, variant) rows -- a plain fact table, not a decision
    /// tree, so a linear scan over it replaces what used to be a 39-arm
    /// `match` (CC 41, the highest in the crate after `Charset::translate`'s
    /// analogous table). Each variant's discriminant already carries this
    /// value (see the enum above), but matching on a `#[repr(u32)]` enum's
    /// own discriminants isn't expressible as a safe transmute, so the pair
    /// is spelled out explicitly instead of relying on layout.
    const RAW_TABLE: &[(u32, Self)] = &[
        (sym::BACKSPACE, Self::BackSpace),
        (sym::TAB, Self::Tab),
        (sym::RETURN, Self::Return),
        (sym::ESCAPE, Self::Escape),
        (sym::DELETE, Self::Delete),
        (sym::INSERT, Self::Insert),
        (sym::HOME, Self::Home),
        (sym::END, Self::End),
        (sym::PAGE_UP, Self::PageUp),
        (sym::PAGE_DOWN, Self::PageDown),
        (sym::LEFT, Self::Left),
        (sym::UP, Self::Up),
        (sym::RIGHT, Self::Right),
        (sym::DOWN, Self::Down),
        (sym::F1, Self::F1),
        (sym::F1 + 1, Self::F2),
        (sym::F1 + 2, Self::F3),
        (sym::F1 + 3, Self::F4),
        (sym::F1 + 4, Self::F5),
        (sym::F1 + 5, Self::F6),
        (sym::F1 + 6, Self::F7),
        (sym::F1 + 7, Self::F8),
        (sym::F1 + 8, Self::F9),
        (sym::F1 + 9, Self::F10),
        (sym::F1 + 10, Self::F11),
        (sym::F12, Self::F12),
        (sym::SHIFT_L, Self::ShiftL),
        (sym::SHIFT_R, Self::ShiftR),
        (sym::CONTROL_L, Self::ControlL),
        (sym::CONTROL_R, Self::ControlR),
        (sym::CAPS_LOCK, Self::CapsLock),
        (sym::NUM_LOCK, Self::NumLock),
        (sym::SCROLL_LOCK, Self::ScrollLock),
        (sym::ALT_L, Self::AltL),
        (sym::ALT_R, Self::AltR),
        (sym::SUPER_L, Self::SuperL),
        (sym::SUPER_R, Self::SuperR),
        (sym::MENU, Self::Menu),
        (sym::KP_ENTER, Self::KpEnter),
    ];

    pub(crate) fn from_raw(raw: u32) -> Option<Self> {
        Self::RAW_TABLE
            .iter()
            .find_map(|&(r, variant)| (r == raw).then_some(variant))
    }
}

/// Extends `XkbKeysym` with its translation to our `Key` enum, as a method
/// instead of a free function -- `XkbKeysym` is the natural owner since this
/// is purely a fold over its own variants.
impl XkbKeysym {
    /// Translate this XKB keysym into our `Key` enum. Split into three
    /// thematic helpers (editing/control keys, navigation, function keys)
    /// plus the modifier fold kept here, each an independent exhaustive
    /// sub-match -- the single 39-arm match this replaced was one function
    /// carrying all of that complexity at once (CC 32) for no reason beyond
    /// "they're all keysym -> Key mappings"; grouped by what a person
    /// actually thinks of as related keys, each group is now independently
    /// readable and (if ever needed) testable.
    pub(crate) const fn to_key(self) -> Key {
        if let Some(key) = self.to_editing_key() {
            return key;
        }
        if let Some(key) = self.to_navigation_key() {
            return key;
        }
        if let Some(key) = self.to_function_key() {
            return key;
        }
        // Unreachable in practice: the three helpers above plus this one
        // collectively cover every `XkbKeysym` variant today (see each
        // one's doc comment for its slice). A new variant added without
        // teaching it to one of the four helpers would panic here rather
        // than silently mis-translating -- loud failure over a wrong
        // keypress.
        self.to_modifier_key()
            .expect("every XkbKeysym variant is covered by one of the 4 helpers")
    }

    /// Backspace/Tab/Enter/Escape/Delete/Insert -- the non-modifier,
    /// non-navigation control keys.
    const fn to_editing_key(self) -> Option<Key> {
        Some(match self {
            Self::BackSpace => Key::Backspace,
            Self::Tab => Key::Tab,
            Self::Return | Self::KpEnter => Key::Enter,
            Self::Escape => Key::Esc,
            Self::Delete => Key::Delete,
            Self::Insert => Key::Insert,
            _ => return None,
        })
    }

    /// Home/End/`PageUp`/`PageDown`/arrow keys.
    const fn to_navigation_key(self) -> Option<Key> {
        Some(match self {
            Self::Home => Key::Home,
            Self::End => Key::End,
            Self::PageUp => Key::PageUp,
            Self::PageDown => Key::PageDown,
            Self::Left => Key::Left,
            Self::Up => Key::Up,
            Self::Right => Key::Right,
            Self::Down => Key::Down,
            _ => return None,
        })
    }

    /// F1..=F12.
    const fn to_function_key(self) -> Option<Key> {
        Some(match self {
            Self::F1 => Key::F1,
            Self::F2 => Key::F2,
            Self::F3 => Key::F3,
            Self::F4 => Key::F4,
            Self::F5 => Key::F5,
            Self::F6 => Key::F6,
            Self::F7 => Key::F7,
            Self::F8 => Key::F8,
            Self::F9 => Key::F9,
            Self::F10 => Key::F10,
            Self::F11 => Key::F11,
            Self::F12 => Key::F12,
            _ => return None,
        })
    }

    /// Shift/Ctrl/`CapsLock`/`NumLock`/`ScrollLock`/Alt/Super/Menu -- the
    /// only variants left once `to_editing_key`/`to_navigation_key`/
    /// `to_function_key` have all missed, so `to_key` calls this last and
    /// unwraps unconditionally.
    const fn to_modifier_key(self) -> Option<Key> {
        Some(match self {
            Self::ShiftL | Self::ShiftR => Key::LShift,
            Self::ControlL | Self::ControlR => Key::LCtrl,
            // No dedicated NumLock/ScrollLock/Menu/Super variants in
            // `protocol::Key` today (only `input::Key` has them) -- these
            // never reach the encoder as anything but modifier-only no-ops
            // either way, so mapping them to the nearest existing modifier
            // variant (rather than dropping the event) keeps
            // repeat-suppression / is_modifier() correct without inventing
            // wire-protocol variants nothing else in `platform` uses.
            Self::CapsLock | Self::NumLock | Self::ScrollLock => Key::CapsLock,
            Self::AltL | Self::AltR => Key::LAlt,
            Self::SuperL | Self::SuperR | Self::Menu => Key::LMeta,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the call-site pattern in `window.rs`'s
    /// `resolve_logical_key`: `XkbKeysym::from_raw` then `to_key`, with no
    /// intermediate free-fn wrapper -- see module docs.
    fn keysym_to_key(raw: u32) -> Option<Key> {
        XkbKeysym::from_raw(raw).map(XkbKeysym::to_key)
    }

    #[test]
    fn niri_swapped_esc_capslock_keysyms_translate_correctly() {
        // The exact keysyms `xkb::Translator::keysym` would hand back for
        // the swapped keymap in `crates/xkb`'s own regression test.
        assert_eq!(keysym_to_key(0xffe5), Some(Key::CapsLock)); // Caps_Lock
        assert_eq!(keysym_to_key(0xff1b), Some(Key::Esc)); // Escape
    }

    #[test]
    fn unmapped_keysym_returns_none() {
        assert_eq!(keysym_to_key(0x1234_5678), None);
    }

    #[test]
    fn kp_enter_and_return_both_map_to_enter() {
        assert_eq!(keysym_to_key(0xff0d), Some(Key::Enter));
        assert_eq!(keysym_to_key(0xff8d), Some(Key::Enter));
    }

    /// The two keysym decoders in this crate must not drift apart.
    ///
    /// They map to *different* target enums on purpose -- this one to
    /// `protocol::Key` (physical keys, modifiers included, for the
    /// terminal) and `toplevel::keysym::Key` to a widget-facing key with no
    /// modifier variants -- so they cannot simply share a function. What
    /// they must share is the *numbers*, which is why both now read
    /// `toplevel::keysym::sym`.
    ///
    /// This test enforces the part a shared constant cannot: that a keysym
    /// both tables claim to know is decoded to the same *meaning* by each.
    /// Adding a key to one table and forgetting the other now fails here
    /// rather than in someone's hands.
    #[test]
    fn both_decoders_agree_on_every_key_they_share() {
        use crate::toplevel::keysym::Key as Decoded;

        // (keysym, what each decoder should independently produce)
        let shared: &[(u32, Key, Decoded)] = &[
            (sym::BACKSPACE, Key::Backspace, Decoded::Backspace),
            (sym::TAB, Key::Tab, Decoded::Tab),
            (sym::RETURN, Key::Enter, Decoded::Enter),
            (sym::KP_ENTER, Key::Enter, Decoded::Enter),
            (sym::ESCAPE, Key::Esc, Decoded::Escape),
            (sym::DELETE, Key::Delete, Decoded::Delete),
            (sym::INSERT, Key::Insert, Decoded::Insert),
            (sym::HOME, Key::Home, Decoded::Home),
            (sym::END, Key::End, Decoded::End),
            (sym::PAGE_UP, Key::PageUp, Decoded::PageUp),
            (sym::PAGE_DOWN, Key::PageDown, Decoded::PageDown),
            (sym::LEFT, Key::Left, Decoded::Left),
            (sym::UP, Key::Up, Decoded::Up),
            (sym::RIGHT, Key::Right, Decoded::Right),
            (sym::DOWN, Key::Down, Decoded::Down),
            (sym::MENU, Key::LMeta, Decoded::Menu),
        ];

        for &(raw, physical, decoded) in shared {
            assert_eq!(
                keysym_to_key(raw),
                Some(physical),
                "physical decoder disagrees on keysym {raw:#x}"
            );
            assert_eq!(
                Decoded::named(raw),
                Some(decoded),
                "widget decoder disagrees on keysym {raw:#x}"
            );
        }
    }

    /// Function keys are a contiguous block in one decoder and twelve
    /// enum variants in the other; walking the whole range catches an
    /// off-by-one that spot-checking F1 and F12 would miss.
    #[test]
    fn the_function_key_block_lines_up_in_both_decoders() {
        use crate::toplevel::keysym::Key as Decoded;

        let physical = [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
        ];
        for (i, expected) in physical.into_iter().enumerate() {
            let raw = sym::F1 + u32::try_from(i).expect("12 fits");
            assert_eq!(keysym_to_key(raw), Some(expected), "physical F{}", i + 1);
            assert_eq!(
                Decoded::named(raw),
                Some(Decoded::Function(u8::try_from(i).expect("12 fits") + 1)),
                "widget F{}",
                i + 1
            );
        }
        assert_eq!(sym::F12, sym::F1 + 11, "the block is exactly 12 keys wide");
    }
}
