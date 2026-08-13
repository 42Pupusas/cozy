//! A hand-built D-Bus frame that declares `UNIX_FDS`, for transport tests.
//!
//! [`crate::message::MessageBuilder`] has no descriptor-bearing constructor
//! yet, because nothing in this workspace sends one. The receive path must
//! still be provable, so this builds the wire bytes a peer would send: a
//! signal whose body is `n` `h` indexes and whose header declares the
//! matching `UNIX_FDS` count.

use crate::message::MessageType;

/// Builds `SIGNAL` frames carrying `h` (unix fd) body values.
///
/// Separate from the test module that uses it so the frame layout stays one
/// responsibility, and so a future descriptor-sending caller has a worked
/// example of the header field to copy.
pub struct FdFrame;

impl FdFrame {
    const UNIX_FDS_FIELD: u8 = 9;
    const SIGNATURE_FIELD: u8 = 8;
    const PATH_FIELD: u8 = 1;
    const INTERFACE_FIELD: u8 = 2;
    const MEMBER_FIELD: u8 = 3;

    /// Build a signal whose body holds `count` `h` indexes, `0..count`, and
    /// whose `UNIX_FDS` header field declares `declared`.
    ///
    /// `declared` is separate from `count` so a test can build the
    /// inconsistent frame a buggy or hostile peer would send.
    #[must_use]
    pub fn signal_with_fds(serial: u32, count: u32, declared: u32) -> Vec<u8> {
        let mut fields = Vec::new();
        Self::write_string_field(&mut fields, Self::PATH_FIELD, b'o', "/org/example");
        Self::write_string_field(&mut fields, Self::INTERFACE_FIELD, b's', "org.example.Fds");
        Self::write_string_field(&mut fields, Self::MEMBER_FIELD, b's', "Handles");

        let signature: String = "h".repeat(count as usize);
        if count > 0 {
            Self::write_string_field(&mut fields, Self::SIGNATURE_FIELD, b'g', &signature);
        }

        Self::pad_to(&mut fields, 8);
        fields.push(Self::UNIX_FDS_FIELD);
        fields.extend_from_slice(&[1, b'u', 0]);
        Self::pad_to(&mut fields, 4);
        fields.extend_from_slice(&declared.to_le_bytes());

        let mut body = Vec::new();
        for index in 0..count {
            body.extend_from_slice(&index.to_le_bytes());
        }

        let mut out = vec![b'l', MessageType::Signal.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&u32::try_from(fields.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        out
    }

    fn write_string_field(buf: &mut Vec<u8>, code: u8, sig_char: u8, text: &str) {
        Self::pad_to(buf, 8);
        buf.push(code);
        buf.extend_from_slice(&[1, sig_char, 0]);
        if sig_char == b'g' {
            buf.push(u8::try_from(text.len()).unwrap_or(0));
        } else {
            Self::pad_to(buf, 4);
            buf.extend_from_slice(&u32::try_from(text.len()).unwrap_or(0).to_le_bytes());
        }
        buf.extend_from_slice(text.as_bytes());
        buf.push(0);
    }

    fn pad_to(buf: &mut Vec<u8>, to: usize) {
        let target = buf.len().div_ceil(to) * to;
        buf.resize(target, 0);
    }
}
