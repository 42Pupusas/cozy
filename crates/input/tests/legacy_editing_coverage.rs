//! Direct coverage for `Encoder::encode_legacy_editing`'s branches: every key
//! it handles, with and without modifiers, and with/without app-cursor mode
//! where that matters (Home/End).

use input::{Encoder, Key, KeyState, Modifiers};

fn enc() -> Encoder {
    Encoder::default()
}

fn encode(e: &Encoder, key: Key, mods: Modifiers) -> Vec<u8> {
    let mut buf = [0u8; 64];
    let n = e
        .encode_key(key, KeyState::Pressed, mods, &mut buf)
        .expect("key should encode");
    buf[..n].to_vec()
}

#[test]
fn enter_is_cr_regardless_of_mods() {
    let e = enc();
    assert_eq!(encode(&e, Key::Enter, Modifiers::NONE), b"\r");
    assert_eq!(encode(&e, Key::Enter, Modifiers::SHIFT), b"\r");
}

#[test]
fn backspace_is_del_byte() {
    let e = enc();
    assert_eq!(encode(&e, Key::Backspace, Modifiers::NONE), b"\x7f");
}

#[test]
fn tab_plain_vs_shift_tab() {
    let e = enc();
    assert_eq!(encode(&e, Key::Tab, Modifiers::NONE), b"\t");
    assert_eq!(encode(&e, Key::Tab, Modifiers::SHIFT), b"\x1b[Z");
}

#[test]
fn esc_key_is_bare_escape() {
    let e = enc();
    assert_eq!(encode(&e, Key::Esc, Modifiers::NONE), b"\x1b");
}

#[test]
fn delete_and_insert_are_tilde_sequences() {
    let e = enc();
    assert_eq!(encode(&e, Key::Delete, Modifiers::NONE), b"\x1b[3~");
    assert_eq!(encode(&e, Key::Insert, Modifiers::NONE), b"\x1b[2~");
}

#[test]
fn home_end_plain_no_app_cursor() {
    let e = enc();
    assert_eq!(encode(&e, Key::Home, Modifiers::NONE), b"\x1b[H");
    assert_eq!(encode(&e, Key::End, Modifiers::NONE), b"\x1b[F");
}

#[test]
fn home_end_plain_with_app_cursor_use_ss3() {
    let mut e = enc();
    e.app_cursor = true;
    assert_eq!(encode(&e, Key::Home, Modifiers::NONE), b"\x1bOH");
    assert_eq!(encode(&e, Key::End, Modifiers::NONE), b"\x1bOF");
}

#[test]
fn home_end_with_mods_use_modified_forms_regardless_of_app_cursor() {
    let mut e = enc();
    e.app_cursor = true;
    // Home uses the CSI-1 form; End uses the CSI-4~ form (its own family).
    assert_eq!(encode(&e, Key::Home, Modifiers::SHIFT), b"\x1b[1;2H");
    assert_eq!(encode(&e, Key::End, Modifiers::CTRL), b"\x1b[4;5~");
}

#[test]
fn page_up_down_plain_and_modified() {
    let e = enc();
    assert_eq!(encode(&e, Key::PageUp, Modifiers::NONE), b"\x1b[5~");
    assert_eq!(encode(&e, Key::PageDown, Modifiers::NONE), b"\x1b[6~");
    assert_eq!(encode(&e, Key::PageUp, Modifiers::SHIFT), b"\x1b[5;2~");
    assert_eq!(encode(&e, Key::PageDown, Modifiers::ALT), b"\x1b[6;3~");
}

#[test]
fn released_editing_keys_produce_nothing_in_legacy_mode() {
    let e = enc();
    let mut buf = [0u8; 16];
    assert_eq!(
        e.encode_key(Key::Home, KeyState::Released, Modifiers::NONE, &mut buf),
        None
    );
}
