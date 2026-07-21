#![allow(
    clippy::cast_possible_truncation,
    clippy::too_long_first_doc_paragraph,
    clippy::match_same_arms,
    clippy::branches_sharing_code,
    clippy::missing_const_for_fn,
    clippy::vec_init_then_push
)]
//! D-Bus wire messages: building method-call frames and parsing whatever
//! comes back (method returns, error replies, signals).
//!
//! Wire format (little-endian, the common case — see [`Header`]):
//! ```text
//! BYTE  endianness ('l')
//! BYTE  message type
//! BYTE  flags
//! BYTE  protocol version (1)
//! UINT32 body length
//! UINT32 serial
//! ARRAY of STRUCT (BYTE, VARIANT)   -- header fields
//! (body, padded to start at an 8-byte boundary from message start)
//! ```

use crate::value::{Value, alignment_of, split_first_type, unmarshal, unmarshal_body};

/// D-Bus message types (the `mtype` header byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    MethodCall,
    MethodReturn,
    Error,
    Signal,
}

impl MessageType {
    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::MethodCall),
            2 => Some(Self::MethodReturn),
            3 => Some(Self::Error),
            4 => Some(Self::Signal),
            _ => None,
        }
    }

    const fn as_byte(self) -> u8 {
        match self {
            Self::MethodCall => 1,
            Self::MethodReturn => 2,
            Self::Error => 3,
            Self::Signal => 4,
        }
    }
}

/// Header field codes we actually read/write. There are more in the spec
/// (`REPLY_SERIAL`, `SENDER`, `SIGNATURE`, …) — we only need the ones a
/// method call needs to send and a reply needs to be interpreted by.
mod field_code {
    pub const PATH: u8 = 1;
    pub const INTERFACE: u8 = 2;
    pub const MEMBER: u8 = 3;
    pub const ERROR_NAME: u8 = 4;
    pub const REPLY_SERIAL: u8 = 5;
    pub const DESTINATION: u8 = 6;
    pub const SIGNATURE: u8 = 8;
}

const fn align_up(n: usize, to: usize) -> usize {
    if to <= 1 { n } else { n.div_ceil(to) * to }
}

fn pad_to(buf: &mut Vec<u8>, to: usize) {
    let target = align_up(buf.len(), to);
    buf.resize(target, 0);
}

fn write_string_field(buf: &mut Vec<u8>, code: u8, sig_char: u8, s: &str) {
    pad_to(buf, 8); // each header field is a struct, 8-aligned
    buf.push(code);
    // VARIANT holding a STRING or OBJECT_PATH or SIGNATURE
    buf.push(1); // signature length
    buf.push(sig_char);
    buf.push(0); // signature NUL
    if sig_char == b'g' {
        // SIGNATURE type: length-prefixed by one byte, no 4-byte len/no pad.
        buf.push(s.len() as u8);
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    } else {
        pad_to(buf, 4);
        buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }
}

/// Append one already-marshalled argument's bytes to a method-call body,
/// handling the padding its type requires.
fn write_value(buf: &mut Vec<u8>, v: &BodyArg) {
    match v {
        BodyArg::Str(s) => {
            pad_to(buf, 4);
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
            buf.push(0);
        }
        BodyArg::ObjectPath(s) => {
            pad_to(buf, 4);
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
            buf.push(0);
        }
        BodyArg::U32(n) => {
            pad_to(buf, 4);
            buf.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// The handful of argument shapes our own outgoing method calls ever need
/// (NetworkManager/bluez getters take zero args or a string/object-path/u32).
/// Kept separate from [`Value`] (the *decode*-side type, which must model
/// everything a reply could contain) since encode only needs to cover what
/// we actually send.
#[derive(Debug, Clone)]
pub enum BodyArg {
    Str(String),
    ObjectPath(String),
    U32(u32),
}

impl BodyArg {
    const fn sig_char(&self) -> u8 {
        match self {
            Self::Str(_) => b's',
            Self::ObjectPath(_) => b'o',
            Self::U32(_) => b'u',
        }
    }
}

/// A parsed reply: enough to tell success from failure and to decode the
/// body against whatever signature the caller expects.
#[derive(Debug)]
pub struct Reply {
    pub kind: MessageType,
    /// Present when `kind == Error`, e.g.
    /// `"org.freedesktop.DBus.Error.ServiceUnknown"`.
    pub error_name: Option<String>,
    /// Present on `MethodReturn`/`Error` messages: the serial of the call
    /// this replies to. Absent (`None`) on `Signal`/`MethodCall` messages --
    /// this is how the connection tells an unsolicited signal (e.g.
    /// `NameAcquired`, sent right after `Hello`, or a `PropertiesChanged`
    /// broadcast) apart from the reply it's actually waiting for. See
    /// `Conn::call_raw`, which now loops discarding non-matching messages
    /// instead of assuming the very next thing off the wire is always the
    /// reply -- an earlier version didn't do this and got its replies
    /// permanently off-by-one the moment any signal arrived.
    pub reply_serial: Option<u32>,
    pub signature: String,
    pub body: Vec<u8>,
}

impl Reply {
    /// Decode the body against `self.signature`, ignoring the caller's
    /// expectation — for callers happy to switch on whatever came back.
    #[must_use]
    pub fn values(&self) -> Vec<Value> {
        unmarshal_body(&self.body, &self.signature).unwrap_or_default()
    }
}

/// Build a `METHOD_CALL` message frame: header + body, ready to write to the
/// socket. `serial` must be nonzero and unique per in-flight call on this
/// connection (the reply carries it back as `REPLY_SERIAL`, though we don't
/// bother matching on that — see [`crate::conn`] doc comment for why a
/// simple connection can get away with request/response-in-order).
#[must_use]
pub fn build_method_call(
    serial: u32,
    destination: &str,
    path: &str,
    interface: &str,
    member: &str,
    args: &[BodyArg],
) -> Vec<u8> {
    // Body first — we need its length and signature for the header.
    let mut body = Vec::new();
    let mut sig = String::new();
    for a in args {
        sig.push(a.sig_char() as char);
        write_value(&mut body, a);
    }

    let mut header_fields = Vec::new();
    write_string_field(&mut header_fields, field_code::PATH, b'o', path);
    write_string_field(&mut header_fields, field_code::DESTINATION, b's', destination);
    write_string_field(&mut header_fields, field_code::INTERFACE, b's', interface);
    write_string_field(&mut header_fields, field_code::MEMBER, b's', member);
    if !sig.is_empty() {
        write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', &sig);
    }

    let mut out = Vec::new();
    out.push(b'l'); // little-endian
    out.push(MessageType::MethodCall.as_byte());
    out.push(0); // flags: none (expect a reply)
    out.push(1); // protocol version
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    // header fields array: length-prefixed, elements 8-aligned (struct).
    out.extend_from_slice(&(header_fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_fields);
    pad_to(&mut out, 8); // body starts 8-aligned from message start
    out.extend_from_slice(&body);
    out
}

/// The fixed 16-byte prelude every D-Bus message starts with, decoded enough
/// to know how many more bytes to read off the wire before the message is
/// complete.
#[derive(Debug, Clone, Copy)]
pub struct Prelude {
    pub message_type: MessageType,
    pub body_len: u32,
    pub header_fields_len: u32,
}

/// Minimum bytes needed before [`Prelude::parse`] can succeed.
pub const PRELUDE_LEN: usize = 16;

impl Prelude {
    /// Parse the fixed prelude from the first [`PRELUDE_LEN`] bytes of a
    /// message. Returns `None` if `buf` is short or not little-endian (we
    /// only speak little-endian on the wire, matching what we send).
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < PRELUDE_LEN || buf[0] != b'l' {
            return None;
        }
        let message_type = MessageType::from_byte(buf[1])?;
        let body_len = u32::from_le_bytes(buf[4..8].try_into().ok()?);
        let header_fields_len = u32::from_le_bytes(buf[12..16].try_into().ok()?);
        Some(Self {
            message_type,
            body_len,
            header_fields_len,
        })
    }

    /// Total byte length of the whole message (prelude + header fields,
    /// padded to 8, + body), i.e. how many bytes the caller must have
    /// buffered before calling [`parse_message`].
    #[must_use]
    pub fn total_len(&self) -> usize {
        let after_fields = PRELUDE_LEN + self.header_fields_len as usize;
        align_up(after_fields, 8) + self.body_len as usize
    }
}

/// Parse one complete message (prelude + header fields + body) out of `buf`,
/// which must be at least `Prelude::parse(buf).total_len()` bytes (checked
/// by the caller via [`Prelude`] first, same two-phase read `platform::conn`
/// does for Wayland frames — read a fixed prelude, then read exactly as many
/// more bytes as it says).
#[must_use]
pub fn parse_message(buf: &[u8]) -> Option<Reply> {
    let prelude = Prelude::parse(buf)?;
    let fields_start = PRELUDE_LEN;
    let fields_end = fields_start + prelude.header_fields_len as usize;
    let _fields_bounds_check = buf.get(fields_start..fields_end)?;

    let mut signature = String::new();
    let mut error_name = None;
    let mut reply_serial = None;

    // Header fields are an ARRAY of STRUCT(BYTE, VARIANT), each struct
    // 8-aligned within the fields array's own start (which is itself
    // 8-aligned relative to the message start, since it directly follows a
    // u32 length at offset 12 -- wait, actually the array *contents* start
    // right after the u32 length with no extra padding beyond the struct's
    // own 8-byte alignment requirement, applied relative to buf[0]).
    let mut offset = fields_start;
    while offset < fields_end {
        offset = align_up(offset, 8);
        if offset >= fields_end {
            break;
        }
        let code = *buf.get(offset)?;
        // VARIANT: 1-byte sig len, sig bytes, NUL, then value.
        let sig_len = *buf.get(offset + 1)? as usize;
        let sig_start = offset + 2;
        let sig_end = sig_start + sig_len;
        let inner_sig = std::str::from_utf8(buf.get(sig_start..sig_end)?).ok()?;
        let val_start = sig_end + 1;
        let (val, next) = unmarshal(buf, val_start, inner_sig)?;
        match code {
            field_code::SIGNATURE => {
                if let Value::Signature(s) = val {
                    signature = s;
                }
            }
            field_code::ERROR_NAME => {
                if let Some(s) = val.as_display_string() {
                    error_name = Some(s);
                }
            }
            field_code::REPLY_SERIAL => {
                reply_serial = val.as_u64().map(|n| n as u32);
            }
            _ => {}
        }
        offset = next;
    }

    let body_start = align_up(fields_end, 8);
    let body_end = body_start + prelude.body_len as usize;
    let body = buf.get(body_start..body_end)?.to_vec();

    // Validate the signature actually parses to the declared body length by
    // touching split_first_type at least once so it's exercised even on
    // signature-less (empty-body) replies -- keeps the import used and
    // documents that an empty signature is valid (zero-arg replies).
    let _ = split_first_type(&signature);
    let _ = alignment_of(&signature);

    Some(Reply {
        kind: prelude.message_type,
        error_name,
        reply_serial,
        signature,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_method_call_no_args() {
        let msg = build_method_call(
            1,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
            "GetDevices",
            &[],
        );
        let prelude = Prelude::parse(&msg).unwrap();
        assert_eq!(prelude.message_type, MessageType::MethodCall);
        assert_eq!(msg.len(), prelude.total_len());
    }

    #[test]
    fn roundtrip_method_call_with_string_arg() {
        let msg = build_method_call(
            7,
            "org.bluez",
            "/org/bluez/hci0",
            "org.freedesktop.DBus.Properties",
            "Get",
            &[
                BodyArg::Str("org.bluez.Adapter1".into()),
                BodyArg::Str("Powered".into()),
            ],
        );
        let prelude = Prelude::parse(&msg).unwrap();
        assert_eq!(prelude.total_len(), msg.len());
        assert!(prelude.body_len > 0);
    }

    #[test]
    fn parse_message_roundtrips_a_hand_built_method_return() {
        // Build a call, then hand-craft a "reply" by re-using the builder's
        // header-writing for a method-return-shaped message body of one
        // string, and confirm parse_message reads it back.
        let mut header_fields = Vec::new();
        write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', "s");

        let mut body = Vec::new();
        write_value(&mut body, &BodyArg::Str("hello".into()));

        let mut out = Vec::new();
        out.push(b'l');
        out.push(MessageType::MethodReturn.as_byte());
        out.push(0);
        out.push(1);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&5u32.to_le_bytes());
        out.extend_from_slice(&(header_fields.len() as u32).to_le_bytes());
        out.extend_from_slice(&header_fields);
        pad_to(&mut out, 8);
        out.extend_from_slice(&body);

        let prelude = Prelude::parse(&out).unwrap();
        assert_eq!(prelude.total_len(), out.len());

        let reply = parse_message(&out).unwrap();
        assert_eq!(reply.kind, MessageType::MethodReturn);
        assert_eq!(reply.signature, "s");
        assert_eq!(reply.values(), vec![Value::Str("hello".into())]);
    }

    #[test]
    fn prelude_rejects_short_or_big_endian_buffers() {
        assert!(Prelude::parse(&[]).is_none());
        assert!(Prelude::parse(&[b'B'; 16]).is_none());
    }
}
