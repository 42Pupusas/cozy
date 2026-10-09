//! [`InputSerial`] -- the serial of the last real user action.
//!
//! # Why this is not just a `u32` field
//!
//! An activation token is only trustworthy because it carries a serial the
//! compositor itself issued for an input event. Which serials qualify is a
//! rule, not a detail: a serial from `wl_pointer.enter` or an `xdg_surface`
//! configure is not evidence that a *user acted*, and presenting one gets
//! the request refused.
//!
//! Recording only presses also matters. A release serial names the end of a
//! gesture, and a compositor that measures "did the user just act" against
//! it sees the wrong moment. So this stores presses and key-downs, and
//! nothing else.

/// The most recent input serial that proves a user acted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputSerial {
    last: u32,
}

impl InputSerial {
    /// A tracker that has seen no input.
    #[must_use]
    pub const fn new() -> Self {
        Self { last: 0 }
    }

    /// The serial to present as evidence, or `None` before any input.
    ///
    /// Zero is not a usable serial -- a compositor issues serials from one
    /// upward -- so "never seen input" is reported as absence rather than as
    /// a serial that would simply be refused.
    #[must_use]
    pub const fn get(self) -> Option<u32> {
        if self.last == 0 {
            None
        } else {
            Some(self.last)
        }
    }

    /// Record a pointer button press.
    ///
    /// Releases are deliberately not recorded; see the module comment.
    pub const fn record_press(&mut self, serial: u32) {
        self.last = serial;
    }

    /// Record a key going down.
    pub const fn record_key_down(&mut self, serial: u32) {
        self.last = serial;
    }
}

#[cfg(test)]
mod tests {
    use super::InputSerial;

    #[test]
    fn a_fresh_tracker_has_no_evidence() {
        assert_eq!(InputSerial::new().get(), None);
    }

    #[test]
    fn a_press_becomes_the_evidence() {
        let mut serial = InputSerial::new();
        serial.record_press(7);
        assert_eq!(serial.get(), Some(7));
    }

    #[test]
    fn a_key_press_counts_too() {
        let mut serial = InputSerial::new();
        serial.record_key_down(12);
        assert_eq!(serial.get(), Some(12));
    }

    /// The newest action is the one the compositor weighs.
    #[test]
    fn the_latest_action_wins_across_both_devices() {
        let mut serial = InputSerial::new();
        serial.record_press(3);
        serial.record_key_down(9);
        assert_eq!(serial.get(), Some(9));
        serial.record_press(11);
        assert_eq!(serial.get(), Some(11));
    }

    /// A compositor issues serials from one upward, so a zero would only be
    /// refused. Reporting absence lets the caller skip the round trip.
    #[test]
    fn a_zero_serial_is_reported_as_no_evidence() {
        let mut serial = InputSerial::new();
        serial.record_press(0);
        assert_eq!(serial.get(), None);
    }
}
