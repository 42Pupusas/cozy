requests! {
    /// `wl_pointer` requests.
    PointerRequest {
        /// Set the cursor surface: `serial`, `surface` (or 0 to hide), `hotspot_x`,
        /// `hotspot_y` (both `i32` in surface-local coords).
        SetCursor = 0,
        /// Release the pointer object (v3+); harmless to skip for our purposes.
        Release = 1,
    }
}
events! {
    /// `wl_pointer` events. Coordinates on `Enter`/`Motion` are `wl_fixed`
    /// (24.8 fixed-point `i32`); see [`Fixed`].
    PointerEvent {
        /// Pointer entered a surface: `serial`, `surface`, `x`, `y`.
        Enter = 0,
        /// Pointer left a surface: `serial`, `surface`.
        Leave = 1,
        /// Pointer moved: `time`, `x`, `y`.
        Motion = 2,
        /// Button changed: `serial`, `time`, `button`, `state`.
        Button = 3,
        /// Scroll/axis event: `time`, `axis`, `value`.
        Axis = 4,
    }
}

/// Mouse buttons, by their Linux evdev codes (as `wl_pointer.button` reports
/// them in its `button` argument).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PointerButton {
    Left = 0x110,
    Right = 0x111,
    Middle = 0x112,
}

impl PointerButton {
    /// Identify a button from its raw evdev code, or `None` if we don't model it.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0x110 => Some(Self::Left),
            0x111 => Some(Self::Right),
            0x112 => Some(Self::Middle),
            _ => None,
        }
    }
}

/// Whether a `wl_pointer.button` event is a press or a release (its `state` arg).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ButtonState {
    Released = 0,
    Pressed = 1,
}

impl ButtonState {
    #[must_use]
    pub const fn from_raw(state: u32) -> Option<Self> {
        match state {
            0 => Some(Self::Released),
            1 => Some(Self::Pressed),
            _ => None,
        }
    }
}

/// A `wl_fixed` value: signed 24.8 fixed-point.
///
/// Pointer coordinates arrive in this format; the newtype keeps them from
/// being read as plain pixels by accident — you must go through
/// [`to_pixels`](Fixed::to_pixels).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fixed(pub i32);

impl Fixed {
    /// Whole pixels, truncating the 8-bit fraction.
    #[must_use]
    pub const fn to_pixels(self) -> i32 {
        self.0 / 256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_button_from_code_known_and_unknown() {
        assert_eq!(PointerButton::from_code(0x110), Some(PointerButton::Left));
        assert_eq!(PointerButton::from_code(0x111), Some(PointerButton::Right));
        assert_eq!(PointerButton::from_code(0x112), Some(PointerButton::Middle));
        assert_eq!(PointerButton::from_code(0x113), None);
        assert_eq!(PointerButton::from_code(0), None);
    }

    #[test]
    fn button_state_from_raw_known_and_unknown() {
        assert_eq!(ButtonState::from_raw(0), Some(ButtonState::Released));
        assert_eq!(ButtonState::from_raw(1), Some(ButtonState::Pressed));
        assert_eq!(ButtonState::from_raw(2), None);
    }

    #[test]
    fn fixed_to_pixels_truncates_toward_zero() {
        assert_eq!(Fixed(256).to_pixels(), 1);
        assert_eq!(Fixed(0).to_pixels(), 0);
        assert_eq!(Fixed(255).to_pixels(), 0);
        assert_eq!(Fixed(-256).to_pixels(), -1);
        assert_eq!(Fixed(-255).to_pixels(), 0);
        assert_eq!(Fixed(1024).to_pixels(), 4);
    }
}
