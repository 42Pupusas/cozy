//! The bar's `wl_keyboard`: it only has focus while a panel popup holds
//! the grab, so every key it resolves is queued for that panel.

use std::os::fd::OwnedFd;

use platform::event::RawEvent;
use platform::id::KeyboardId;
use platform::key_repeat::KeyRepeatState;
use platform::keyboard_decoder::{KeyInput, KeyboardDecoder, KeyboardUpdate};
use platform::protocol::Key;
use platform::sys::RepeatTimer;

use crate::popup_key::PanelKey;

pub struct BarKeyboard {
    id: Option<KeyboardId>,
    decoder: KeyboardDecoder,
    repeat: KeyRepeatState,
    held: Option<PanelKey>,
    pending: Vec<PanelKey>,
}

impl BarKeyboard {
    #[must_use]
    pub fn new(timer: RepeatTimer) -> Self {
        Self {
            id: None,
            decoder: KeyboardDecoder::new(),
            repeat: KeyRepeatState::new(timer),
            held: None,
            pending: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> Option<KeyboardId> {
        self.id
    }

    pub const fn set_id(&mut self, id: KeyboardId) {
        self.id = Some(id);
    }

    pub fn absorb_keymap_fd(&mut self, fds: &[OwnedFd]) {
        self.decoder.absorb_keymap_fd(fds);
    }

    pub fn on_event(&mut self, ev: &RawEvent) {
        match self.decoder.observe(ev) {
            KeyboardUpdate::Key(input) => self.on_key(input),
            KeyboardUpdate::Repeat(info) => self.repeat.set_repeat_info(info.rate, info.delay_ms),
            KeyboardUpdate::Left => self.release_all(),
            KeyboardUpdate::Ignored | KeyboardUpdate::Entered | KeyboardUpdate::Modifiers(_) => {}
        }
    }

    pub fn on_repeat_tick(&mut self) {
        if self.repeat.held_key().is_none() {
            return;
        }
        if let Some(key) = self.held {
            self.pending.push(key);
        }
    }

    pub fn take_keys(&mut self) -> Vec<PanelKey> {
        std::mem::take(&mut self.pending)
    }

    /// Forget held and queued keys, for when the popup they were meant
    /// for goes away.
    pub fn release_all(&mut self) {
        self.held = None;
        self.pending.clear();
        self.repeat.on_leave();
    }

    fn on_key(&mut self, input: KeyInput) {
        let code = Key::from_code(input.evdev);
        if !input.is_press() {
            if let Some(k) = code {
                if self.repeat.held_key() == Some(k) {
                    self.held = None;
                }
                self.repeat.on_key_release(k);
            }
            return;
        }
        let Some(resolved) = PanelKey::from_key(input.key) else {
            return;
        };
        self.pending.push(resolved);
        if let Some(k) = code
            && !k.is_modifier()
        {
            self.held = Some(resolved);
            self.repeat.on_key_press(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::protocol::KeyState;

    fn keyboard() -> BarKeyboard {
        BarKeyboard::new(RepeatTimer::new().expect("timerfd"))
    }

    fn input(state: KeyState, key: platform::toplevel::keysym::Key) -> KeyInput {
        KeyInput {
            evdev: Key::A as u32,
            state,
            keysym: 0,
            key: Some(key),
            ctrl: false,
        }
    }

    #[test]
    fn a_press_queues_its_key_once() {
        let mut kb = keyboard();
        kb.on_key(input(
            KeyState::Pressed,
            platform::toplevel::keysym::Key::Char('a'),
        ));
        assert_eq!(kb.take_keys(), [PanelKey::Char('a')]);
        assert!(kb.take_keys().is_empty());
    }

    #[test]
    fn a_held_key_repeats_until_released() {
        let mut kb = keyboard();
        let a = platform::toplevel::keysym::Key::Char('a');
        kb.on_key(input(KeyState::Pressed, a));
        kb.take_keys();
        kb.on_repeat_tick();
        assert_eq!(kb.take_keys(), [PanelKey::Char('a')]);
        kb.on_key(input(KeyState::Released, a));
        kb.on_repeat_tick();
        assert!(kb.take_keys().is_empty());
    }

    #[test]
    fn release_all_drops_queued_and_held_keys() {
        let mut kb = keyboard();
        kb.on_key(input(
            KeyState::Pressed,
            platform::toplevel::keysym::Key::Char('a'),
        ));
        kb.release_all();
        kb.on_repeat_tick();
        assert!(kb.take_keys().is_empty());
    }
}
