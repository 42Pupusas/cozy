//! Key and modifier types — platform-agnostic.
//!
//! These are **not** evdev keycodes; they are a normalised semantic layer.
//! The `platform` crate converts evdev codes → [`Key`] before handing events
//! to us.  That indirection means the encoder stays portable across Wayland,
//! future backends, and test harnesses.

/// A modifier bitmask.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers(pub u8);

impl Modifiers {
    pub const NONE:    Self = Self(0b0000_0000);
    pub const SHIFT:   Self = Self(0b0000_0001);
    pub const ALT:     Self = Self(0b0000_0010);
    pub const CTRL:    Self = Self(0b0000_0100);
    pub const SUPER:   Self = Self(0b0000_1000);
    pub const CAPS:    Self = Self(0b0001_0000);
    pub const NUM:     Self = Self(0b0010_0000);

    #[inline] #[must_use] pub fn shift(self) -> bool  { self.0 & 0x01 != 0 }
    #[inline] #[must_use] pub fn alt(self) -> bool    { self.0 & 0x02 != 0 }
    #[inline] #[must_use] pub fn ctrl(self) -> bool   { self.0 & 0x04 != 0 }
    #[inline] #[must_use] pub fn sup(self) -> bool    { self.0 & 0x08 != 0 }
    #[inline] #[must_use] pub fn caps(self) -> bool   { self.0 & 0x10 != 0 }
    #[inline] #[must_use] pub fn num(self) -> bool    { self.0 & 0x20 != 0 }

    /// The Kitty/xterm modifier parameter: `shift+1`, `alt+2`, `ctrl+4`,
    /// `super+8` — value is `(bitset + 1)` so 1 = no modifiers (omitted in
    /// legacy mode). Returns `None` when no modifiers are active.
    #[must_use]
    pub fn kitty_param(self) -> Option<u8> {
        let bits = (self.0 & 0x0f) as u8; // shift/alt/ctrl/super only
        if bits == 0 { None } else { Some(bits + 1) }
    }

    /// Whether any of shift/alt/ctrl/super is set.
    #[inline] #[must_use]
    pub fn any_active(self) -> bool { self.0 & 0x0f != 0 }
}

impl core::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
}

/// Whether a key was pressed or released (or repeated — the OS auto-repeat).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyState {
    Pressed,
    Released,
    Repeated,
}

/// A normalised semantic key — after layout mapping, before VT encoding.
///
/// Printable characters (letters, digits, symbols) are carried as a `char`
/// in [`Key::Char`].  Special keys get their own variants.  The `platform`
/// crate maps evdev codes + modifiers → this type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable Unicode scalar (letter, digit, punctuation, emoji).
    Char(char),

    // ── Control cluster ──────────────────────────────────────────────────
    Enter,
    Backspace,
    Tab,
    Esc,
    Delete,
    Insert,

    // ── Navigation ───────────────────────────────────────────────────────
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,

    // ── Function keys ────────────────────────────────────────────────────
    F(u8), // F1–F25

    // ── Keypad ───────────────────────────────────────────────────────────
    KpEnter,
    KpUp,
    KpDown,
    KpLeft,
    KpRight,
    KpHome,
    KpEnd,
    KpPageUp,
    KpPageDown,
    KpInsert,
    KpDelete,
    KpBegin,
    KpChar(char), // '0'–'9', '.', '+', '-', '*', '/'

    // ── Modifier-only keys (produce no bytes in legacy mode) ─────────────
    Shift,
    Ctrl,
    Alt,
    Super,
    CapsLock,
    NumLock,
    ScrollLock,
    Menu,
}

impl Key {
    /// True if this key produces no output in legacy VT mode (modifier-only keys).
    #[must_use]
    pub fn is_modifier_only(self) -> bool {
        matches!(
            self,
            Self::Shift
                | Self::Ctrl
                | Self::Alt
                | Self::Super
                | Self::CapsLock
                | Self::NumLock
                | Self::ScrollLock
                | Self::Menu
        )
    }
}
