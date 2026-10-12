//! When the picker may take exclusive keyboard focus.
//!
//! An exclusive grab without a keymap routes every key to a client that
//! cannot decode Escape, which leaves the user unable to dismiss the overlay.
//! The grab is therefore requested only once a keymap is installed.

use platform::protocol::KeyboardInteractivity;

/// Tracks whether the exclusive grab has been requested yet.
#[derive(Debug, PartialEq, Eq)]
pub struct KeyboardGrab {
    state: GrabState,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum GrabState {
    NoKeyboard,
    AwaitingKeymap,
    Granted,
}

impl KeyboardGrab {
    #[must_use]
    pub const fn new(has_keyboard: bool) -> Self {
        let state = if has_keyboard {
            GrabState::AwaitingKeymap
        } else {
            GrabState::NoKeyboard
        };
        Self { state }
    }

    /// The interactivity the surface is created with.
    #[must_use]
    pub const fn initial() -> KeyboardInteractivity {
        KeyboardInteractivity::None
    }

    /// The interactivity to switch to now that the decoder reports
    /// `has_keymap`, or `None` when nothing should change.
    pub const fn on_keymap(&mut self, has_keymap: bool) -> Option<KeyboardInteractivity> {
        match (self.state, has_keymap) {
            (GrabState::AwaitingKeymap, true) => {
                self.state = GrabState::Granted;
                Some(KeyboardInteractivity::Exclusive)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KeyboardGrab;
    use platform::protocol::KeyboardInteractivity;

    #[test]
    fn the_surface_starts_without_keyboard_focus() {
        assert_eq!(KeyboardGrab::initial(), KeyboardInteractivity::None);
    }

    #[test]
    fn a_keymap_grants_the_exclusive_grab_once() {
        let mut grab = KeyboardGrab::new(true);
        assert_eq!(grab.on_keymap(true), Some(KeyboardInteractivity::Exclusive));
        assert_eq!(grab.on_keymap(true), None);
    }

    #[test]
    fn a_failed_keymap_never_grabs() {
        let mut grab = KeyboardGrab::new(true);
        assert_eq!(grab.on_keymap(false), None);
    }

    #[test]
    fn no_keyboard_never_grabs() {
        let mut grab = KeyboardGrab::new(false);
        assert_eq!(grab.on_keymap(true), None);
    }
}
