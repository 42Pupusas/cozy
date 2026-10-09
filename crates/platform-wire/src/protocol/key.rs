/// The keys we act on, by their Linux evdev codes.
///
/// Exactly what a `wl_keyboard.key` event reports in its `key` argument (the
/// raw kernel keycode; the +8 XKB offset applies only to the keymap, not the
/// wire field).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Key {
    // Row 1: number row
    Backtick = 41,
    N1 = 2,
    N2 = 3,
    N3 = 4,
    N4 = 5,
    N5 = 6,
    N6 = 7,
    N7 = 8,
    N8 = 9,
    N9 = 10,
    N0 = 11,
    Minus = 12,
    Equal = 13,
    Backspace = 14,
    // Row 2: QWERTY
    Tab = 15,
    Q = 16,
    W = 17,
    E = 18,
    R = 19,
    T = 20,
    Y = 21,
    U = 22,
    I = 23,
    O = 24,
    P = 25,
    LBracket = 26,
    RBracket = 27,
    Backslash = 43,
    // Row 3: home row
    CapsLock = 58,
    A = 30,
    S = 31,
    D = 32,
    F = 33,
    G = 34,
    H = 35,
    J = 36,
    K = 37,
    L = 38,
    Semicolon = 39,
    Apostrophe = 40,
    Enter = 28,
    // Row 4: bottom row
    LShift = 42,
    Z = 44,
    X = 45,
    C = 46,
    V = 47,
    B = 48,
    N = 49,
    M = 50,
    Comma = 51,
    Dot = 52,
    Slash = 53,
    RShift = 54,
    // Modifiers / special
    LCtrl = 29,
    LMeta = 125,
    LAlt = 56,
    Space = 57,
    RAlt = 100,
    RMeta = 126,
    RCtrl = 97,
    // Navigation cluster
    Esc = 1,
    F1 = 59,
    F2 = 60,
    F3 = 61,
    F4 = 62,
    F5 = 63,
    F6 = 64,
    F7 = 65,
    F8 = 66,
    F9 = 67,
    F10 = 68,
    F11 = 87,
    F12 = 88,
    Insert = 110,
    Delete = 111,
    Home = 102,
    End = 107,
    PageUp = 104,
    PageDown = 109,
    Up = 103,
    Down = 108,
    Left = 105,
    Right = 106,
}

impl Key {
    /// Identify a key from its raw evdev code, or `None` if we don't recognise it.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Esc),
            2 => Some(Self::N1),
            3 => Some(Self::N2),
            4 => Some(Self::N3),
            5 => Some(Self::N4),
            6 => Some(Self::N5),
            7 => Some(Self::N6),
            8 => Some(Self::N7),
            9 => Some(Self::N8),
            10 => Some(Self::N9),
            11 => Some(Self::N0),
            12 => Some(Self::Minus),
            13 => Some(Self::Equal),
            14 => Some(Self::Backspace),
            15 => Some(Self::Tab),
            16 => Some(Self::Q),
            17 => Some(Self::W),
            18 => Some(Self::E),
            19 => Some(Self::R),
            20 => Some(Self::T),
            21 => Some(Self::Y),
            22 => Some(Self::U),
            23 => Some(Self::I),
            24 => Some(Self::O),
            25 => Some(Self::P),
            26 => Some(Self::LBracket),
            27 => Some(Self::RBracket),
            28 => Some(Self::Enter),
            29 => Some(Self::LCtrl),
            30 => Some(Self::A),
            31 => Some(Self::S),
            32 => Some(Self::D),
            33 => Some(Self::F),
            34 => Some(Self::G),
            35 => Some(Self::H),
            36 => Some(Self::J),
            37 => Some(Self::K),
            38 => Some(Self::L),
            39 => Some(Self::Semicolon),
            40 => Some(Self::Apostrophe),
            41 => Some(Self::Backtick),
            42 => Some(Self::LShift),
            43 => Some(Self::Backslash),
            44 => Some(Self::Z),
            45 => Some(Self::X),
            46 => Some(Self::C),
            47 => Some(Self::V),
            48 => Some(Self::B),
            49 => Some(Self::N),
            50 => Some(Self::M),
            51 => Some(Self::Comma),
            52 => Some(Self::Dot),
            53 => Some(Self::Slash),
            54 => Some(Self::RShift),
            56 => Some(Self::LAlt),
            57 => Some(Self::Space),
            58 => Some(Self::CapsLock),
            59 => Some(Self::F1),
            60 => Some(Self::F2),
            61 => Some(Self::F3),
            62 => Some(Self::F4),
            63 => Some(Self::F5),
            64 => Some(Self::F6),
            65 => Some(Self::F7),
            66 => Some(Self::F8),
            67 => Some(Self::F9),
            68 => Some(Self::F10),
            87 => Some(Self::F11),
            88 => Some(Self::F12),
            97 => Some(Self::RCtrl),
            100 => Some(Self::RAlt),
            102 => Some(Self::Home),
            103 => Some(Self::Up),
            104 => Some(Self::PageUp),
            105 => Some(Self::Left),
            106 => Some(Self::Right),
            107 => Some(Self::End),
            108 => Some(Self::Down),
            109 => Some(Self::PageDown),
            110 => Some(Self::Insert),
            111 => Some(Self::Delete),
            125 => Some(Self::LMeta),
            126 => Some(Self::RMeta),
            _ => None,
        }
    }

    /// Modifier keys (Ctrl/Alt/Shift/Meta) never auto-repeat — holding them
    /// must not synthesize a stream of key events.
    #[must_use]
    pub const fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::LCtrl
                | Self::RCtrl
                | Self::LAlt
                | Self::RAlt
                | Self::LShift
                | Self::RShift
                | Self::LMeta
                | Self::RMeta
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_KEY_CODES: &[u32] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
        49, 50, 51, 52, 53, 54, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88, 97,
        100, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 125, 126,
    ];

    #[test]
    fn key_from_code_recognises_every_documented_code_and_roundtrips() {
        for &code in VALID_KEY_CODES {
            let key = Key::from_code(code)
                .unwrap_or_else(|| panic!("code {code} should decode to a Key"));
            assert_eq!(
                key as u32, code,
                "Key discriminant must equal its evdev code"
            );
        }
    }

    #[test]
    fn key_from_code_rejects_unknown_codes() {
        for code in [
            0,
            55,
            69,
            86,
            89,
            96,
            98,
            99,
            101,
            112,
            124,
            127,
            200,
            1000,
            u32::MAX,
        ] {
            assert_eq!(Key::from_code(code), None, "code {code} should not decode");
        }
    }

    #[test]
    fn key_from_code_covers_every_code_in_0_to_200_consistently() {
        for code in 0..=200u32 {
            let expected_some = VALID_KEY_CODES.contains(&code);
            assert_eq!(
                Key::from_code(code).is_some(),
                expected_some,
                "code {code} disagreed with expected validity"
            );
        }
    }

    #[test]
    fn key_is_modifier_true_only_for_the_eight_modifier_keys() {
        let modifiers = [
            Key::LCtrl,
            Key::RCtrl,
            Key::LAlt,
            Key::RAlt,
            Key::LShift,
            Key::RShift,
            Key::LMeta,
            Key::RMeta,
        ];
        for &k in &modifiers {
            assert!(k.is_modifier(), "{k:?} should be a modifier");
        }
        for &k in &[Key::A, Key::Enter, Key::Space, Key::F1, Key::Up, Key::Esc] {
            assert!(!k.is_modifier(), "{k:?} should not be a modifier");
        }
    }
}
