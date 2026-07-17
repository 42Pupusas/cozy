//! Probes for suspected edge-case bugs in `input`'s encoder: mouse modes,
//! coordinate overflow, non-ASCII text, and kitty modifier combinations.

use input::encode::{Encoder, KeyboardMode};
use input::key::{Key, KeyState, Modifiers};
use input::mouse::{ButtonState, MouseButton, MouseEvent, MouseMode};

fn encode_key(enc: &Encoder, key: Key, mods: Modifiers) -> Vec<u8> {
    let mut b = [0u8; 64];
    let n = enc
        .encode_key(key, KeyState::Pressed, mods, &mut b)
        .unwrap_or(0);
    b[..n].to_vec()
}

/// BUG PROBE: an already-uppercase `Key::Char('A')` with no Shift modifier
/// reported should still type 'A' verbatim (this is what a real key event
/// looks like when caps comes from the OS layout, not our Shift bit).
/// `unshifted_char` only has lowercase-letter arms, so uppercase ASCII falls
/// through to the " " catch-all.
#[test]
fn uppercase_char_without_shift_mod_is_not_mangled_to_space() {
    let e = Encoder::new();
    let out = encode_key(&e, Key::Char('A'), Modifiers::NONE);
    assert_eq!(out, b"A", "typing 'A' must not become a space");
}

/// BUG PROBE: non-ASCII printable characters (accented letters, CJK, emoji)
/// must be encoded verbatim as UTF-8, not collapsed to a single space.
#[test]
fn non_ascii_char_is_encoded_verbatim() {
    let e = Encoder::new();
    let out = encode_key(&e, Key::Char('é'), Modifiers::NONE);
    assert_eq!(out, "é".as_bytes(), "non-ASCII char must round-trip");
}

#[test]
fn emoji_char_is_encoded_verbatim() {
    let e = Encoder::new();
    let out = encode_key(&e, Key::Char('🎉'), Modifiers::NONE);
    assert_eq!(out, "🎉".as_bytes());
}

#[test]
fn cjk_char_is_encoded_verbatim() {
    let e = Encoder::new();
    let out = encode_key(&e, Key::Char('漢'), Modifiers::NONE);
    assert_eq!(out, "漢".as_bytes());
}

/// X10 mouse mode wheel-up must encode with the wheel bit (64), not be
/// silently treated as a plain button.
#[test]
fn x10_wheel_up_encodes_wheel_code() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::X10;
    let ev = MouseEvent {
        button: MouseButton::WheelUp,
        state: ButtonState::Pressed,
        col: 0,
        row: 0,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    let n = e.encode_mouse(ev, &mut b).unwrap();
    // CSI M <64+32> <1+32> <1+32>
    assert_eq!(&b[..n], &[0x1b, b'[', b'M', 64 + 32, 1 + 32, 1 + 32]);
}

#[test]
fn x10_wheel_down_encodes_wheel_code() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::X10;
    let ev = MouseEvent {
        button: MouseButton::WheelDown,
        state: ButtonState::Pressed,
        col: 0,
        row: 0,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    let n = e.encode_mouse(ev, &mut b).unwrap();
    assert_eq!(&b[..n], &[0x1b, b'[', b'M', 65 + 32, 1 + 32, 1 + 32]);
}

/// X10-mode coordinates beyond 223 (255-32) must clamp, not wrap the u8 add.
#[test]
fn x10_coord_beyond_223_clamps_does_not_wrap() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::Normal;
    let ev = MouseEvent {
        button: MouseButton::Left,
        state: ButtonState::Pressed,
        col: 5000, // way beyond the 1-byte X10 coordinate space
        row: 5000,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    let n = e.encode_mouse(ev, &mut b).unwrap();
    // col/row clamp to 223, then +32 = 255, not wrap around to something small.
    assert_eq!(b[4], 255);
    assert_eq!(b[5], 255);
    let _ = n;
}

/// SGR mode has no coordinate cap — must carry a large column through
/// verbatim (its whole point vs. X10).
#[test]
fn sgr_coord_beyond_223_is_not_clamped() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::Sgr;
    let ev = MouseEvent {
        button: MouseButton::Left,
        state: ButtonState::Pressed,
        col: 5000,
        row: 3000,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 32];
    let n = e.encode_mouse(ev, &mut b).unwrap();
    let s = std::str::from_utf8(&b[..n]).unwrap();
    assert_eq!(s, "\x1b[<0;5001;3001M");
}

/// `ButtonMotion` / `AnyEvent` modes must still encode via the X10-style path
/// (not silently dropped as unsupported).
#[test]
fn button_motion_mode_encodes() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::ButtonMotion;
    let ev = MouseEvent {
        button: MouseButton::Left,
        state: ButtonState::Pressed,
        col: 1,
        row: 1,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    assert!(e.encode_mouse(ev, &mut b).is_some());
}

#[test]
fn any_event_mode_encodes() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::AnyEvent;
    let ev = MouseEvent {
        button: MouseButton::Middle,
        state: ButtonState::Released,
        col: 1,
        row: 1,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    assert!(e.encode_mouse(ev, &mut b).is_some());
}

/// Extended (side) mouse buttons must carry through their code, not panic
/// on the `128 + n` arithmetic for a large `n`.
#[test]
fn extended_button_high_index_does_not_panic() {
    let mut e = Encoder::new();
    e.mouse_mode = MouseMode::Sgr;
    let ev = MouseEvent {
        button: MouseButton::Extended(127),
        state: ButtonState::Pressed,
        col: 0,
        row: 0,
        mods: Modifiers::NONE,
    };
    let mut b = [0u8; 16];
    let n = e.encode_mouse(ev, &mut b).unwrap();
    let s = std::str::from_utf8(&b[..n]).unwrap();
    assert!(s.starts_with("\x1b[<255;"));
}

/// All four modifiers together must combine correctly in the kitty param
/// (shift=1, alt=2, ctrl=4, super=8 -> sum+1 = 15).
#[test]
fn kitty_all_modifiers_combine() {
    let mut e = Encoder::new();
    e.keyboard_mode = KeyboardMode::Kitty;
    let mods = Modifiers::SHIFT | Modifiers::ALT | Modifiers::CTRL | Modifiers::SUPER;
    let out = encode_key(&e, Key::Char('a'), mods);
    let s = std::str::from_utf8(&out).unwrap();
    assert_eq!(s, "\x1b[97;16:1u");
}

/// Repeated key state must be reported distinctly (`event_type=2`) in kitty
/// mode, not collapsed into press.
#[test]
fn kitty_repeated_state_reports_event_type_2() {
    let e_kitty = {
        let mut e = Encoder::new();
        e.keyboard_mode = KeyboardMode::Kitty;
        e
    };
    let mut b = [0u8; 32];
    let n = e_kitty
        .encode_key(Key::Char('a'), KeyState::Repeated, Modifiers::NONE, &mut b)
        .unwrap();
    let s = std::str::from_utf8(&b[..n]).unwrap();
    assert_eq!(s, "\x1b[97;1:2u");
}

/// F-keys beyond F12 (F13..F25) have no legacy or kitty mapping in this
/// encoder yet — must return `None` gracefully, not panic.
#[test]
fn fkey_13_returns_none_not_panic() {
    let e = Encoder::new();
    let mut b = [0u8; 32];
    let r = e.encode_key(Key::F(13), KeyState::Pressed, Modifiers::NONE, &mut b);
    assert!(r.is_none());
}

#[test]
fn fkey_25_returns_none_not_panic() {
    let e = Encoder::new();
    let mut b = [0u8; 32];
    let r = e.encode_key(Key::F(25), KeyState::Pressed, Modifiers::NONE, &mut b);
    assert!(r.is_none());
}

/// Released state in legacy mode must produce nothing for ordinary keys
/// (legacy VT has no key-up reporting).
#[test]
fn legacy_released_key_produces_nothing() {
    let e = Encoder::new();
    let mut b = [0u8; 32];
    let r = e.encode_key(Key::Char('a'), KeyState::Released, Modifiers::NONE, &mut b);
    assert!(r.is_none());
}

/// xterm modifyOtherKeys: an unmodified key must fall back to the plain
/// legacy byte, not the `CSI 27` escape form.
#[test]
fn xterm_modify_unmodified_falls_back_to_legacy() {
    let mut e = Encoder::new();
    e.keyboard_mode = KeyboardMode::XtermModifyOtherKeys;
    let out = encode_key(&e, Key::Char('a'), Modifiers::NONE);
    assert_eq!(out, b"a");
}

#[test]
fn xterm_modify_with_ctrl_uses_csi27_form() {
    let mut e = Encoder::new();
    e.keyboard_mode = KeyboardMode::XtermModifyOtherKeys;
    let out = encode_key(&e, Key::Char('a'), Modifiers::CTRL);
    let s = std::str::from_utf8(&out).unwrap();
    assert_eq!(s, "\x1b[27;5;97~");
}
