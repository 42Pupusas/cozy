use super::Interface;

pub const WL_SEAT: Interface = Interface {
    name: "wl_seat",
    // v4+ so the keyboard delivers `repeat_info` (key-repeat rate/delay). The
    // keyboard object inherits the seat's bound version.
    version: 4,
};

requests! {
    /// `wl_seat` requests — an input-device group (pointer/keyboard/touch).
    /// Touch (`get_touch` = 2) comes when we add that device.
    SeatRequest {
        GetPointer = 0,
        GetKeyboard = 1,
    }
}
events! {
    /// `wl_seat` events.
    SeatEvent {
        /// Bitmask of available device capabilities (see [`SeatCapability`]).
        Capabilities = 0,
        /// A human-readable seat name.
        Name = 1,
    }
}

/// `wl_seat` capability bits (from the `capabilities` event bitmask).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SeatCapability {
    Pointer = 1,
    Keyboard = 2,
    Touch = 4,
}

impl SeatCapability {
    /// Whether `bitmask` (from the capabilities event) includes this capability.
    #[must_use]
    pub const fn in_mask(self, bitmask: u32) -> bool {
        bitmask & (self as u32) != 0
    }

    /// This capability as a one-bit mask, for building a bitmask up rather
    /// than only testing one apart.
    #[must_use]
    pub const fn mask(self) -> u32 {
        self as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seat_capability_in_mask_checks_individual_bits() {
        let mask = SeatCapability::Pointer as u32 | SeatCapability::Touch as u32;
        assert!(SeatCapability::Pointer.in_mask(mask));
        assert!(!SeatCapability::Keyboard.in_mask(mask));
        assert!(SeatCapability::Touch.in_mask(mask));
        assert!(!SeatCapability::Pointer.in_mask(0));
    }
}
