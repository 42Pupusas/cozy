requests! {
    /// `wl_keyboard` requests.
    KeyboardRequest {
        /// Release the keyboard object (v3+); harmless to skip for our purposes.
        Release = 0,
    }
}
events! {
    /// `wl_keyboard` events.
    KeyboardEvent {
        /// The XKB keymap, as `format`, an `fd`, and a `size`. The fd rides in
        /// ancillary data — the one keyboard event that carries one. Parsed
        /// via `crate::xkb::Translator` (see `window.rs`'s
        /// `handle_clipboard_fds`, which despite the name also correlates
        /// this fd by arrival order) so `Key` events honor the compositor's
        /// actual keymap — remaps included — instead of only the raw
        /// physical evdev code.
        Keymap = 0,
        /// Keyboard focus entered a surface: `serial`, `surface`, `keys` (an
        /// array of the evdev codes already held down).
        Enter = 1,
        /// Keyboard focus left a surface: `serial`, `surface`.
        Leave = 2,
        /// A key changed: `serial`, `time`, `key` (evdev code), `state`.
        Key = 3,
        /// Modifier/group state changed: `serial`, `depressed`, `latched`,
        /// `locked`, `group`. Tracked in `TermWindow::modifiers`, and (when
        /// an XKB translator is loaded) fed straight through to
        /// `xkb_state_update_mask` so keysym resolution reflects the live
        /// modifier/layout state.
        Modifiers = 4,
        /// The compositor's key-repeat parameters: `rate` (keys/sec, 0 disables
        /// repeat) and `delay` (ms before the first repeat). `wl_keyboard` v4+.
        RepeatInfo = 5,
    }
}

/// Whether a `wl_keyboard.key` event is a press or a release (its `state` arg).
///
/// Same wire values as [`ButtonState`](super::ButtonState), but a distinct
/// type so a key state can't be passed where a pointer-button state is
/// expected.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum KeyState {
    Released = 0,
    Pressed = 1,
}

impl KeyState {
    #[must_use]
    pub const fn from_raw(state: u32) -> Option<Self> {
        match state {
            0 => Some(Self::Released),
            1 => Some(Self::Pressed),
            _ => None,
        }
    }
}

/// Keyboard modifier state, decoded from `wl_keyboard.modifiers`.
///
/// The depressed/latched/locked fields are bitmasks of the XKB modifier
/// indices (not Linux evdev keycodes). We track the union — any modifier
/// that's depressed, latched, *or* locked is considered active.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Raw depressed modifier bitmask.
    pub depressed: u32,
    /// Raw latched modifier bitmask (e.g. one-shot Shift).
    pub latched: u32,
    /// Raw locked modifier bitmask (e.g. Caps Lock).
    pub locked: u32,
    /// Layout group index.
    pub group: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_state_from_raw_known_and_unknown() {
        assert_eq!(KeyState::from_raw(0), Some(KeyState::Released));
        assert_eq!(KeyState::from_raw(1), Some(KeyState::Pressed));
        assert_eq!(KeyState::from_raw(2), None);
    }
}
