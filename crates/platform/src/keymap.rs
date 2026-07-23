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
    BackSpace = 0xff08,
    Tab = 0xff09,
    Return = 0xff0d,
    Escape = 0xff1b,
    Delete = 0xffff,
    Insert = 0xff63,
    Home = 0xff50,
    End = 0xff57,
    PageUp = 0xff55,
    PageDown = 0xff56,
    Left = 0xff51,
    Up = 0xff52,
    Right = 0xff53,
    Down = 0xff54,
    F1 = 0xffbe,
    F2 = 0xffbf,
    F3 = 0xffc0,
    F4 = 0xffc1,
    F5 = 0xffc2,
    F6 = 0xffc3,
    F7 = 0xffc4,
    F8 = 0xffc5,
    F9 = 0xffc6,
    F10 = 0xffc7,
    F11 = 0xffc8,
    F12 = 0xffc9,
    ShiftL = 0xffe1,
    ShiftR = 0xffe2,
    ControlL = 0xffe3,
    ControlR = 0xffe4,
    CapsLock = 0xffe5,
    NumLock = 0xff7f,
    ScrollLock = 0xff14,
    AltL = 0xffe9,
    AltR = 0xffea,
    SuperL = 0xffeb,
    SuperR = 0xffec,
    Menu = 0xff67,
    KpEnter = 0xff8d,
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
        (0xff08, Self::BackSpace),
        (0xff09, Self::Tab),
        (0xff0d, Self::Return),
        (0xff1b, Self::Escape),
        (0xffff, Self::Delete),
        (0xff63, Self::Insert),
        (0xff50, Self::Home),
        (0xff57, Self::End),
        (0xff55, Self::PageUp),
        (0xff56, Self::PageDown),
        (0xff51, Self::Left),
        (0xff52, Self::Up),
        (0xff53, Self::Right),
        (0xff54, Self::Down),
        (0xffbe, Self::F1),
        (0xffbf, Self::F2),
        (0xffc0, Self::F3),
        (0xffc1, Self::F4),
        (0xffc2, Self::F5),
        (0xffc3, Self::F6),
        (0xffc4, Self::F7),
        (0xffc5, Self::F8),
        (0xffc6, Self::F9),
        (0xffc7, Self::F10),
        (0xffc8, Self::F11),
        (0xffc9, Self::F12),
        (0xffe1, Self::ShiftL),
        (0xffe2, Self::ShiftR),
        (0xffe3, Self::ControlL),
        (0xffe4, Self::ControlR),
        (0xffe5, Self::CapsLock),
        (0xff7f, Self::NumLock),
        (0xff14, Self::ScrollLock),
        (0xffe9, Self::AltL),
        (0xffea, Self::AltR),
        (0xffeb, Self::SuperL),
        (0xffec, Self::SuperR),
        (0xff67, Self::Menu),
        (0xff8d, Self::KpEnter),
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
}
