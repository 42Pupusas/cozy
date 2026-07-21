//! Keyboard auto-repeat state for the terminal window.
//!
//! The compositor only ever sends key *transitions* (press/release) --
//! synthesising the repeated presses while a key is held is the client's
//! job, paced by whatever rate/delay the compositor's `repeat_info` event
//! announced. This owns that whole picture: the currently-held key, the
//! negotiated rate/delay, and the `timerfd` ([`crate::sys::RepeatTimer`])
//! that fires while it's held. Split out of `TermWindow` so the handful of
//! "which key is repeating, and is the timer armed to match" invariants
//! live and are named in one place instead of three loose fields the window
//! had to keep in lockstep by hand.

use crate::protocol::Key;
use crate::sys::RepeatTimer;

/// How fast a held key repeats and after how long it starts, until the
/// compositor's first `repeat_info` event overrides them. xterm's classic
/// defaults (also this project's pre-`repeat_info` baseline).
const DEFAULT_REPEAT_RATE: u32 = 25;
const DEFAULT_REPEAT_DELAY: u32 = 600;

pub struct KeyRepeatState {
    timer: RepeatTimer,
    /// Repeats per second, from the compositor's `repeat_info` (arg 0).
    rate: u32,
    /// Milliseconds before the first repeat, from `repeat_info` (arg 1).
    delay: u32,
    /// The key currently auto-repeating, if any -- also doubles as "is the
    /// timer armed", since the two are always kept in lockstep by this type.
    held_key: Option<Key>,
}

impl KeyRepeatState {
    /// Wrap an already-created timerfd (owned by the connection setup, same
    /// as before this type existed) with fresh rate/delay/held-key state.
    pub const fn new(timer: RepeatTimer) -> Self {
        Self {
            timer,
            rate: DEFAULT_REPEAT_RATE,
            delay: DEFAULT_REPEAT_DELAY,
            held_key: None,
        }
    }

    /// The key that should repeat right now, if the timer just fired
    /// (`RawEvent::Repeat`). `None` means nothing is held (a stray/late
    /// expiry racing a release -- safe to ignore).
    pub const fn held_key(&self) -> Option<Key> {
        self.held_key
    }

    /// A non-modifier key was pressed: arm the timer (first expiry after
    /// `delay`, then every `interval`) and remember it as the repeating key.
    /// Modifier keys never repeat -- the caller filters those out before
    /// calling this (see `Key::is_modifier`).
    ///
    /// A failed arm leaves `held_key` set but the timer not actually
    /// running: annoying (the key just won't auto-repeat) but not fatal, so
    /// this doesn't abort the event loop -- it only logs, same as the
    /// original inline code.
    pub fn on_key_press(&mut self, key: Key) {
        self.held_key = Some(key);
        if let Err(e) = self
            .timer
            .arm(u64::from(self.delay), u64::from(1000 / self.rate.max(1)))
        {
            eprintln!("platform: failed to arm key-repeat timer: {e}");
        }
    }

    /// The currently-repeating key was released: disarm the timer and clear
    /// `held_key`. No-op if `key` isn't the one currently repeating (e.g. a
    /// modifier release, or a stale event for a key that already stopped).
    pub fn on_key_release(&mut self, key: Key) {
        if self.held_key != Some(key) {
            return;
        }
        self.held_key = None;
        if let Err(e) = self.timer.disarm() {
            eprintln!("platform: failed to disarm key-repeat timer: {e}");
        }
    }

    /// `wl_keyboard.leave`: focus is gone, so no key can still be "held" from
    /// the window's perspective (we won't see its release). Disarm and
    /// forget whatever was repeating.
    pub fn on_leave(&mut self) {
        self.held_key = None;
        if let Err(e) = self.timer.disarm() {
            eprintln!("platform: failed to disarm key-repeat timer: {e}");
        }
    }

    /// `wl_keyboard.repeat_info`: the compositor announced (or changed) the
    /// rate/delay. Takes effect on the next [`Self::on_key_press`] -- an
    /// in-flight repeat keeps its already-armed schedule, matching the
    /// original behavior (the timer was never re-armed mid-hold on this
    /// event).
    pub const fn set_repeat_info(&mut self, rate: u32, delay: u32) {
        self.rate = rate;
        self.delay = delay;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> KeyRepeatState {
        KeyRepeatState::new(RepeatTimer::new().expect("timerfd_create"))
    }

    #[test]
    fn new_has_no_held_key() {
        assert_eq!(state().held_key(), None);
    }

    #[test]
    fn press_sets_held_key() {
        let mut s = state();
        s.on_key_press(Key::A);
        assert_eq!(s.held_key(), Some(Key::A));
    }

    #[test]
    fn release_of_the_held_key_clears_it() {
        let mut s = state();
        s.on_key_press(Key::A);
        s.on_key_release(Key::A);
        assert_eq!(s.held_key(), None);
    }

    #[test]
    fn release_of_a_different_key_is_ignored() {
        let mut s = state();
        s.on_key_press(Key::A);
        s.on_key_release(Key::B);
        assert_eq!(s.held_key(), Some(Key::A));
    }

    #[test]
    fn leave_clears_held_key_even_without_a_release() {
        let mut s = state();
        s.on_key_press(Key::A);
        s.on_leave();
        assert_eq!(s.held_key(), None);
    }

    #[test]
    fn set_repeat_info_does_not_disturb_held_key() {
        let mut s = state();
        s.on_key_press(Key::A);
        s.set_repeat_info(33, 400);
        assert_eq!(s.held_key(), Some(Key::A));
    }

    #[test]
    fn a_second_press_replaces_the_held_key() {
        let mut s = state();
        s.on_key_press(Key::A);
        s.on_key_press(Key::B);
        assert_eq!(s.held_key(), Some(Key::B));
    }
}
