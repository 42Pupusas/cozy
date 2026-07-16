//! Keymap translation: evdev scancodes → logical key symbols.
//!
//! ## Current state (first-iteration stub)
//!
//! We operate on raw Linux evdev keycodes — exactly what `wl_keyboard.key`
//! reports on the wire. This is sufficient for:
//!
//! * Movement keys (WASD, arrows)
//! * Modifier-aware action keys (Space, Tab, Esc, Enter)
//! * Simple text entry (US-QWERTY only, no layout awareness)
//!
//! What does **not** work with physical-only keys:
//!
//! * Non-US layouts (AZERTY, QWERTZ, Dvorak, …)
//! * Keys that differ by layout (e.g. `KEY_Z` is 'Y' on QWERTZ)
//! * Dead keys / compose sequences
//! * IME / CJK input
//!
//! ## Future: XKB integration
//!
//! When we need full keyboard support, the path is:
//!
//! 1. Parse the `wl_keyboard.keymap` fd (XKB keymap, v1 format) — the fd is
//!    sent as ancillary data in the `keymap` event.
//! 2. Feed it to `xkbcommon` (via `xkbcommon-dl`, already in winit's dep tree
//!    for the X11 backend).
//! 3. `xkb_state_key_get_one_sym()` maps each `key+modifiers` to a keysym.
//! 4. Keysyms can then be classified: is this a letter? A dead key? Does it
//!    produce text? This enables `ReceivedCharacter` events for text input.
//!
//! The stub below keeps the API stable so callers don't change when we swap in
//! the real implementation.

use crate::protocol::Key;

/// Extends [`Key`] with physical→logical keymap resolution, as a method
/// instead of a free `physical_to_logical(key)` function.
pub trait PhysicalKey {
    /// Map this physical evdev keycode to its logical keysym. When XKB is
    /// integrated, this will look up the keycode in the compiled keymap and
    /// return a logical keysym.
    fn to_logical(self) -> Self;
}

impl PhysicalKey for Key {
    fn to_logical(self) -> Self {
        // Stub: physical-only. Replace with xkb_state_key_get_one_sym().
        self
    }
}
