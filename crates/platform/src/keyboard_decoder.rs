use std::os::fd::{AsFd, OwnedFd};

use crate::event::RawEvent;
use crate::protocol::{KeyState, KeyboardEvent};
use crate::toplevel::keyboard::RepeatInfo;
use crate::toplevel::keysym::Key;
use crate::toplevel::modifiers::ModifierState;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub evdev: u32,
    pub state: KeyState,
    pub keysym: u32,
    pub key: Option<Key>,
    pub ctrl: bool,
}

impl KeyInput {
    #[must_use]
    pub fn is_press(&self) -> bool {
        self.state == KeyState::Pressed
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyboardUpdate {
    Ignored,
    Entered,
    Left,
    Modifiers(ModifierState),
    Repeat(RepeatInfo),
    Key(KeyInput),
}

const MESSAGE_HEADER_BYTES: usize = 8;
const MODIFIERS_GROUP_OFFSET: usize = MESSAGE_HEADER_BYTES + 4 * 4;

#[derive(Default)]
pub struct KeyboardDecoder {
    translator: Option<xkb::Translator>,
    awaiting_keymap_size: Option<usize>,
}

impl KeyboardDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn has_keymap(&self) -> bool {
        self.translator.is_some()
    }

    #[must_use]
    pub const fn is_awaiting_keymap_fd(&self) -> bool {
        self.awaiting_keymap_size.is_some()
    }

    pub fn observe(&mut self, ev: &RawEvent) -> KeyboardUpdate {
        match ev.event::<KeyboardEvent>() {
            Some(KeyboardEvent::Keymap) => {
                self.awaiting_keymap_size = Some(ev.arg(1) as usize);
                KeyboardUpdate::Ignored
            }
            Some(KeyboardEvent::Enter) => KeyboardUpdate::Entered,
            Some(KeyboardEvent::Leave) => KeyboardUpdate::Left,
            Some(KeyboardEvent::Modifiers) => self.apply_modifiers(ev),
            Some(KeyboardEvent::RepeatInfo) => KeyboardUpdate::Repeat(RepeatInfo {
                rate: ev.arg(0),
                delay_ms: ev.arg(1),
            }),
            Some(KeyboardEvent::Key) => self.decode_key(ev),
            None => KeyboardUpdate::Ignored,
        }
    }

    pub fn absorb_keymap_fd(&mut self, fds: &[OwnedFd]) -> bool {
        let Some(size) = self.awaiting_keymap_size.take() else {
            return false;
        };
        let Some(fd) = fds.first() else {
            return true;
        };
        match xkb::Translator::from_keymap_fd(fd.as_fd(), size) {
            Ok(translator) => self.translator = Some(translator),
            Err(e) => eprintln!("platform: XKB keymap unavailable: {e}"),
        }
        true
    }

    fn modifiers_group(ev: &RawEvent) -> u32 {
        ev.wire_bytes()
            .and_then(|bytes| bytes.get(MODIFIERS_GROUP_OFFSET..MODIFIERS_GROUP_OFFSET + 4))
            .and_then(|word| <[u8; 4]>::try_from(word).ok())
            .map_or(0, u32::from_ne_bytes)
    }

    fn apply_modifiers(&mut self, ev: &RawEvent) -> KeyboardUpdate {
        let modifiers =
            ModifierState::from_wire(ev.arg(1), ev.arg(2), ev.arg(3), Self::modifiers_group(ev));
        if let Some(translator) = &mut self.translator {
            let (depressed, latched, locked, group) = modifiers.raw();
            let _ = translator.update_mask(depressed, latched, locked, group);
        }
        KeyboardUpdate::Modifiers(modifiers)
    }

    fn decode_key(&self, ev: &RawEvent) -> KeyboardUpdate {
        let Some(state) = KeyState::from_raw(ev.arg(3)) else {
            return KeyboardUpdate::Ignored;
        };
        let evdev = ev.arg(2);
        let Some(translator) = &self.translator else {
            return KeyboardUpdate::Key(KeyInput {
                evdev,
                state,
                keysym: 0,
                key: None,
                ctrl: false,
            });
        };
        let keysym = translator.keysym(evdev);
        KeyboardUpdate::Key(KeyInput {
            evdev,
            state,
            keysym,
            key: Key::decode(keysym, translator.utf32(evdev)),
            ctrl: translator.control_active(),
        })
    }

    #[cfg(test)]
    fn with_keymap_text(text: &str) -> Option<Self> {
        xkb::Translator::from_keymap_text(text)
            .ok()
            .map(|translator| Self {
                translator: Some(translator),
                awaiting_keymap_size: None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const KEYMAP_OPCODE: u16 = 0;
    const KEY_OPCODE: u16 = 3;
    const MODIFIERS_OPCODE: u16 = 4;
    const REPEAT_INFO_OPCODE: u16 = 5;
    const KEYBOARD_OBJECT: u32 = 9;
    const PHYSICAL_ESC: u32 = 1;
    const PHYSICAL_CAPS: u32 = 58;

    const CAPS_AS_ESCAPE: &str = r#"xkb_keymap {
  xkb_keycodes "remap" {
    minimum = 8;
    maximum = 255;
    <ESC> = 9;
    <CAPS> = 66;
    <AC01> = 38;
  };
  xkb_types "(unnamed)" { include "complete" };
  xkb_compat "(unnamed)" { include "complete" };
  xkb_symbols "remap" {
    key <ESC> { [ Caps_Lock ] };
    key <CAPS> { [ Escape ] };
    key <AC01> { [ a, A ] };
  };
};
"#;

    fn wire(opcode: u16, args: &[u32]) -> RawEvent {
        let size = u32::try_from(MESSAGE_HEADER_BYTES + args.len() * 4).expect("small message");
        let mut message = Vec::new();
        message.extend_from_slice(&KEYBOARD_OBJECT.to_ne_bytes());
        message.extend_from_slice(&opcode.to_ne_bytes());
        message.extend_from_slice(&u16::try_from(size).expect("small message").to_ne_bytes());
        for arg in args {
            message.extend_from_slice(&arg.to_ne_bytes());
        }
        RawEvent::from_wire_msg(&message)
    }

    fn key_event(evdev: u32, state: KeyState) -> RawEvent {
        wire(KEY_OPCODE, &[1, 0, evdev, state as u32])
    }

    fn keymap_fd(text: &str) -> OwnedFd {
        let path = std::env::temp_dir().join(format!(
            "keyboard-decoder-{}-{}",
            std::process::id(),
            text.len()
        ));
        let mut file = std::fs::File::create(&path).expect("create keymap file");
        file.write_all(text.as_bytes()).expect("write keymap");
        file.write_all(b"\0").expect("write terminator");
        drop(file);
        let reopened = std::fs::File::open(&path).expect("reopen keymap file");
        std::fs::remove_file(&path).expect("unlink keymap file");
        OwnedFd::from(reopened)
    }

    #[test]
    fn a_remapped_key_resolves_to_what_the_keymap_says() {
        let Some(mut decoder) = KeyboardDecoder::with_keymap_text(CAPS_AS_ESCAPE) else {
            return;
        };
        let KeyboardUpdate::Key(input) =
            decoder.observe(&key_event(PHYSICAL_CAPS, KeyState::Pressed))
        else {
            panic!("expected a key update");
        };
        assert_eq!(input.key, Some(Key::Escape));
        assert!(input.is_press());
    }

    #[test]
    fn the_physical_escape_key_is_not_escape_once_remapped() {
        let Some(mut decoder) = KeyboardDecoder::with_keymap_text(CAPS_AS_ESCAPE) else {
            return;
        };
        let KeyboardUpdate::Key(input) =
            decoder.observe(&key_event(PHYSICAL_ESC, KeyState::Pressed))
        else {
            panic!("expected a key update");
        };
        assert_ne!(input.key, Some(Key::Escape));
    }

    #[test]
    fn text_keys_resolve_to_characters() {
        let Some(mut decoder) = KeyboardDecoder::with_keymap_text(CAPS_AS_ESCAPE) else {
            return;
        };
        let KeyboardUpdate::Key(input) = decoder.observe(&key_event(30, KeyState::Pressed)) else {
            panic!("expected a key update");
        };
        assert_eq!(input.key, Some(Key::Char('a')));
    }

    #[test]
    fn keys_before_a_keymap_arrives_have_no_logical_key() {
        let mut decoder = KeyboardDecoder::new();
        let KeyboardUpdate::Key(input) =
            decoder.observe(&key_event(PHYSICAL_ESC, KeyState::Pressed))
        else {
            panic!("expected a key update");
        };
        assert_eq!(input.key, None);
        assert_eq!(input.evdev, PHYSICAL_ESC);
        assert!(!decoder.has_keymap());
    }

    #[test]
    fn an_unknown_key_state_is_ignored() {
        let mut decoder = KeyboardDecoder::new();
        assert_eq!(
            decoder.observe(&wire(KEY_OPCODE, &[1, 0, 30, 7])),
            KeyboardUpdate::Ignored
        );
    }

    #[test]
    fn a_keymap_event_parks_its_size_until_the_fd_arrives() {
        let mut decoder = KeyboardDecoder::new();
        assert!(!decoder.is_awaiting_keymap_fd());
        assert_eq!(
            decoder.observe(&wire(KEYMAP_OPCODE, &[1, 4096])),
            KeyboardUpdate::Ignored
        );
        assert!(decoder.is_awaiting_keymap_fd());
    }

    #[test]
    fn the_keymap_fd_installs_the_remap() {
        let mut decoder = KeyboardDecoder::new();
        let size = CAPS_AS_ESCAPE.len() + 1;
        let _ = decoder.observe(&wire(
            KEYMAP_OPCODE,
            &[1, u32::try_from(size).expect("small keymap")],
        ));
        let fds = [keymap_fd(CAPS_AS_ESCAPE)];
        assert!(decoder.absorb_keymap_fd(&fds));
        assert!(!decoder.is_awaiting_keymap_fd());
        if !decoder.has_keymap() {
            return;
        }
        let KeyboardUpdate::Key(input) =
            decoder.observe(&key_event(PHYSICAL_CAPS, KeyState::Pressed))
        else {
            panic!("expected a key update");
        };
        assert_eq!(input.key, Some(Key::Escape));
    }

    #[test]
    fn a_fd_nobody_asked_for_is_left_alone() {
        let mut decoder = KeyboardDecoder::new();
        assert!(!decoder.absorb_keymap_fd(&[keymap_fd("x")]));
    }

    #[test]
    fn modifiers_and_repeat_info_are_decoded() {
        let mut decoder = KeyboardDecoder::new();
        assert_eq!(
            decoder.observe(&wire(MODIFIERS_OPCODE, &[1, 4, 0, 2, 1])),
            KeyboardUpdate::Modifiers(ModifierState::from_wire(4, 0, 2, 1))
        );
        assert_eq!(
            decoder.observe(&wire(REPEAT_INFO_OPCODE, &[30, 250])),
            KeyboardUpdate::Repeat(RepeatInfo {
                rate: 30,
                delay_ms: 250
            })
        );
    }
}
