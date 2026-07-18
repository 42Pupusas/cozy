//! Mouse event types and encoding mode.

/// Which mouse button was involved.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// Scroll up (treated as a button in X10/Normal modes).
    WheelUp,
    /// Scroll down.
    WheelDown,
    /// Extended button 4–7 (side buttons, etc.).
    Extended(u8),
}

impl MouseButton {
    /// Raw X10 button code (before modifier bits are OR'd in).
    #[must_use]
    pub const fn x10_code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::Extended(n) => 128 + n,
        }
    }
}

/// Whether the button was pressed or released.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ButtonState {
    Pressed,
    Released,
}

/// A complete mouse event from the platform layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub state: ButtonState,
    /// Column of the cell the pointer is over (0-based).
    pub col: u16,
    /// Row of the cell the pointer is over (0-based).
    pub row: u16,
    /// Active modifiers at the time of the event.
    pub mods: crate::key::Modifiers,
}

/// Which mouse-reporting mode is active (negotiated by the running application).
///
/// The terminal grid sets this when it processes `CSI ? … h/l`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    /// No mouse reporting (default).
    #[default]
    Off,
    /// X10 compatibility: press-only, cell coordinates, 3 bytes.
    X10,
    /// Normal: press + release, cell coordinates, 3 bytes.  Button-event
    /// tracking (`CSI ? 1000 h`).
    Normal,
    /// Button-event tracking + motion while pressed (`CSI ? 1002 h`).
    ButtonMotion,
    /// Any-event tracking: motion even without buttons (`CSI ? 1003 h`).
    AnyEvent,
    /// SGR encoding (`CSI ? 1006 h`): unlimited coordinates, unambiguous
    /// press/release, variable length.
    Sgr,
}

impl MouseMode {
    /// Pack into one byte for cross-thread transport (e.g. an `AtomicU8`
    /// shuttling the grid's negotiated mode to the UI thread's `Encoder`).
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::X10 => 1,
            Self::Normal => 2,
            Self::ButtonMotion => 3,
            Self::AnyEvent => 4,
            Self::Sgr => 5,
        }
    }

    /// Inverse of [`Self::to_u8`]. Any unrecognized byte decodes to `Off`
    /// (fail safe -- never accidentally reports mouse events an app didn't
    /// ask for).
    #[must_use]
    pub const fn from_u8(b: u8) -> Self {
        match b {
            1 => Self::X10,
            2 => Self::Normal,
            3 => Self::ButtonMotion,
            4 => Self::AnyEvent,
            5 => Self::Sgr,
            _ => Self::Off,
        }
    }
}
