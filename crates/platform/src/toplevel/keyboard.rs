//! Keyboard state: focus, modifiers, and key repeat.
//!
//! Splitting this from the window is what makes repeat testable. In
//! `TermWindow` the repeat rules live inside the `io_uring` loop, entangled
//! with a timerfd, so "does a second key press cancel the first key's
//! repeat" can only be answered by typing at a real terminal. Here the
//! state machine is told the time rather than reading it, so every rule
//! below is a unit test.

use crate::toplevel::keysym::Key;
use crate::toplevel::modifiers::ModifierState;

/// Compositor default when it never sends `repeat_info`: 25 keys/sec after
/// a 600ms delay, matching what most compositors advertise.
const DEFAULT_RATE: u32 = 25;
const DEFAULT_DELAY_MS: u32 = 600;

/// A key event handed to the owner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub modifiers: ModifierState,
    /// Whether this came from auto-repeat rather than a physical press.
    ///
    /// Exposed because some actions must not repeat: holding Enter should
    /// not open a file fifty times, while holding Down should absolutely
    /// keep moving the cursor.
    pub repeat: bool,
}

/// How the compositor wants keys to repeat.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RepeatInfo {
    /// Repeats per second. Zero disables repeat entirely.
    pub rate: u32,
    /// Milliseconds to hold before the first repeat.
    pub delay_ms: u32,
}

impl Default for RepeatInfo {
    fn default() -> Self {
        Self {
            rate: DEFAULT_RATE,
            delay_ms: DEFAULT_DELAY_MS,
        }
    }
}

impl RepeatInfo {
    /// Interval between repeats once started, in milliseconds.
    ///
    /// Returns `None` when repeat is disabled, so the caller cannot divide
    /// by a zero rate.
    #[must_use]
    pub const fn interval_ms(self) -> Option<u32> {
        if self.rate == 0 {
            return None;
        }
        // At least 1ms: a pathological rate of 2000 would otherwise round to
        // a zero-length timer and spin the event loop.
        let ms = 1000 / self.rate;
        Some(if ms == 0 { 1 } else { ms })
    }
}

/// The key currently repeating, if any.
#[derive(Copy, Clone, Debug)]
struct Repeating {
    key: Key,
    modifiers: ModifierState,
    /// Timestamp (ms, monotonic) at which the next repeat is due.
    next_due_ms: u64,
    /// Whether the initial delay has elapsed and we are now in the fast
    /// phase. Kept explicit so the first interval can differ from the rest.
    started: bool,
}

/// Tracks keyboard focus, modifier state, and which key is repeating.
#[derive(Debug, Default)]
pub struct KeyboardState {
    focused: bool,
    modifiers: ModifierState,
    repeat_info: RepeatInfo,
    /// Exactly one key repeats -- the most recent one down.
    ///
    /// Not a set: every toolkit repeats only the last key, because holding
    /// `j` then also holding `k` should switch to repeating `k`, not
    /// interleave both.
    repeating: Option<Repeating>,
}

impl KeyboardState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this window currently has keyboard focus.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// The live modifier state.
    #[must_use]
    pub const fn modifiers(&self) -> ModifierState {
        self.modifiers
    }

    /// The compositor's repeat parameters.
    #[must_use]
    pub const fn repeat_info(&self) -> RepeatInfo {
        self.repeat_info
    }

    /// Whether a key is currently repeating.
    #[must_use]
    pub const fn is_repeating(&self) -> bool {
        self.repeating.is_some()
    }

    /// `wl_keyboard.enter`.
    pub const fn enter(&mut self) {
        self.focused = true;
    }

    /// `wl_keyboard.leave`.
    ///
    /// Repeat stops: the key-up will be delivered to whoever has focus now,
    /// so a key held across a focus change would otherwise repeat forever.
    pub const fn leave(&mut self) {
        self.focused = false;
        self.repeating = None;
    }

    /// `wl_keyboard.modifiers`.
    pub const fn set_modifiers(&mut self, modifiers: ModifierState) {
        self.modifiers = modifiers;
    }

    /// `wl_keyboard.repeat_info`.
    pub const fn set_repeat_info(&mut self, info: RepeatInfo) {
        self.repeat_info = info;
        // A rate of zero disables repeat; drop anything in flight rather
        // than letting it continue under the old rate.
        if info.rate == 0 {
            self.repeating = None;
        }
    }

    /// A key went down. `now_ms` is a monotonic timestamp.
    ///
    /// Returns the press to dispatch, or `None` if the key means nothing
    /// here (a bare modifier, an unmapped key).
    pub fn press(&mut self, key: Option<Key>, now_ms: u64) -> Option<KeyPress> {
        let key = key?;
        // Arm repeat for this key, replacing whatever was repeating. Text
        // and navigation both repeat; the caller decides what to ignore via
        // `KeyPress::repeat`.
        if let Some(interval) = self.repeat_info.interval_ms() {
            let _ = interval;
            self.repeating = Some(Repeating {
                key,
                modifiers: self.modifiers,
                next_due_ms: now_ms + u64::from(self.repeat_info.delay_ms),
                started: false,
            });
        }
        Some(KeyPress {
            key,
            modifiers: self.modifiers,
            repeat: false,
        })
    }

    /// A key came up.
    ///
    /// Only cancels repeat if *this* is the repeating key. Releasing a
    /// different key must not stop it -- otherwise letting go of Shift while
    /// holding Down freezes the cursor.
    pub fn release(&mut self, key: Option<Key>) {
        let Some(key) = key else { return };
        if self.repeating.map(|r| r.key) == Some(key) {
            self.repeating = None;
        }
    }

    /// When the next repeat is due, if one is pending.
    ///
    /// The window uses this to arm its timer; `None` means disarm.
    #[must_use]
    pub const fn next_repeat_due_ms(&self) -> Option<u64> {
        match self.repeating {
            Some(r) => Some(r.next_due_ms),
            None => None,
        }
    }

    /// Advance the clock and emit a repeat if one is due.
    ///
    /// Called when the repeat timer fires. Returns at most one press per
    /// call: if the process stalled and several intervals elapsed, emitting
    /// all of them would produce a burst of keystrokes the user never
    /// typed, so the schedule is reset relative to *now* instead.
    pub fn tick(&mut self, now_ms: u64) -> Option<KeyPress> {
        let interval = self.repeat_info.interval_ms()?;
        let r = self.repeating.as_mut()?;
        if now_ms < r.next_due_ms {
            return None;
        }
        r.started = true;
        r.next_due_ms = now_ms + u64::from(interval);
        Some(KeyPress {
            key: r.key,
            modifiers: r.modifiers,
            repeat: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods_with_shift() -> ModifierState {
        ModifierState::from_wire(1, 0, 0, 0)
    }

    #[test]
    fn a_fresh_keyboard_is_unfocused_and_idle() {
        let k = KeyboardState::new();
        assert!(!k.is_focused());
        assert!(!k.is_repeating());
        assert_eq!(k.next_repeat_due_ms(), None);
    }

    #[test]
    fn focus_is_tracked() {
        let mut k = KeyboardState::new();
        k.enter();
        assert!(k.is_focused());
        k.leave();
        assert!(!k.is_focused());
    }

    #[test]
    fn a_press_reports_the_key_and_live_modifiers() {
        let mut k = KeyboardState::new();
        k.set_modifiers(mods_with_shift());
        let press = k.press(Some(Key::Char('a')), 0).expect("a press");
        assert_eq!(press.key, Key::Char('a'));
        assert!(press.modifiers.shift());
        assert!(!press.repeat);
    }

    #[test]
    fn an_unmapped_key_produces_nothing() {
        let mut k = KeyboardState::new();
        assert!(k.press(None, 0).is_none());
        assert!(!k.is_repeating());
    }

    #[test]
    fn repeat_waits_for_the_delay_then_fires_at_the_interval() {
        let mut k = KeyboardState::new();
        k.set_repeat_info(RepeatInfo {
            rate: 25,
            delay_ms: 600,
        });
        k.press(Some(Key::Down), 0);

        // Nothing before the delay elapses.
        assert!(k.tick(599).is_none());

        let first = k.tick(600).expect("first repeat");
        assert_eq!(first.key, Key::Down);
        assert!(first.repeat, "a repeat must be marked as one");

        // 25/sec is a 40ms interval.
        assert!(k.tick(639).is_none());
        assert!(k.tick(640).is_some());
    }

    /// The bug most likely to escape review: typing fast leaves the previous
    /// key repeating forever if a new press does not take over.
    #[test]
    fn a_new_press_takes_over_the_repeat_from_the_previous_key() {
        let mut k = KeyboardState::new();
        k.press(Some(Key::Char('a')), 0);
        k.press(Some(Key::Char('b')), 10);

        let due = k.next_repeat_due_ms().expect("still repeating");
        assert_eq!(due, 10 + 600, "the schedule restarts from the new press");

        let rep = k.tick(due).expect("a repeat");
        assert_eq!(
            rep.key,
            Key::Char('b'),
            "the newest key repeats, not the old"
        );
    }

    #[test]
    fn releasing_the_repeating_key_stops_it() {
        let mut k = KeyboardState::new();
        k.press(Some(Key::Down), 0);
        k.release(Some(Key::Down));
        assert!(!k.is_repeating());
        assert!(k.tick(10_000).is_none());
    }

    /// Releasing Shift while holding Down is routine; it must not freeze the
    /// cursor.
    #[test]
    fn releasing_a_different_key_leaves_the_repeat_running() {
        let mut k = KeyboardState::new();
        k.press(Some(Key::Down), 0);
        k.release(Some(Key::Char('x')));
        assert!(k.is_repeating());
        assert!(k.tick(600).is_some());
    }

    #[test]
    fn losing_focus_stops_repeat() {
        let mut k = KeyboardState::new();
        k.enter();
        k.press(Some(Key::Down), 0);
        k.leave();
        assert!(!k.is_repeating());
        assert!(k.tick(10_000).is_none());
    }

    #[test]
    fn a_zero_rate_disables_repeat_entirely() {
        let mut k = KeyboardState::new();
        k.set_repeat_info(RepeatInfo {
            rate: 0,
            delay_ms: 600,
        });
        k.press(Some(Key::Down), 0);
        assert!(!k.is_repeating());
        assert!(k.tick(10_000).is_none());
    }

    #[test]
    fn a_zero_rate_arriving_late_cancels_a_live_repeat() {
        let mut k = KeyboardState::new();
        k.press(Some(Key::Down), 0);
        assert!(k.is_repeating());
        k.set_repeat_info(RepeatInfo {
            rate: 0,
            delay_ms: 0,
        });
        assert!(!k.is_repeating());
    }

    /// A stalled process must not dump a burst of keystrokes the user never
    /// typed.
    #[test]
    fn a_long_stall_yields_one_repeat_not_a_burst() {
        let mut k = KeyboardState::new();
        k.press(Some(Key::Down), 0);
        assert!(k.tick(60_000).is_some());
        // The schedule is rebased on now, so the next one is an interval
        // away rather than immediately due.
        assert!(k.tick(60_000).is_none());
        assert!(k.tick(60_040).is_some());
    }

    #[test]
    fn a_pathological_rate_never_produces_a_zero_length_interval() {
        let info = RepeatInfo {
            rate: 2000,
            delay_ms: 0,
        };
        assert_eq!(info.interval_ms(), Some(1));
    }

    #[test]
    fn a_disabled_rate_has_no_interval() {
        let info = RepeatInfo {
            rate: 0,
            delay_ms: 600,
        };
        assert_eq!(info.interval_ms(), None);
    }

    #[test]
    fn the_default_matches_the_common_compositor_advertisement() {
        let info = RepeatInfo::default();
        assert_eq!(info.rate, 25);
        assert_eq!(info.delay_ms, 600);
    }

    #[test]
    fn a_repeat_carries_the_modifiers_from_when_the_key_went_down() {
        let mut k = KeyboardState::new();
        k.set_modifiers(mods_with_shift());
        k.press(Some(Key::Down), 0);
        // Shift comes up, but the held key keeps repeating as shifted --
        // matching the physical reality that the keystroke was shifted.
        k.set_modifiers(ModifierState::default());
        let rep = k.tick(600).expect("a repeat");
        assert!(rep.modifiers.shift());
    }
}
