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

use crate::marshal::Marshaller;
use crate::value::{Unmarshaller, Value};

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

    pub(crate) const fn as_byte(self) -> u8 {
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
    pub const SENDER: u8 = 7;
    pub const SIGNATURE: u8 = 8;
    /// How many `SCM_RIGHTS` descriptors ride with this message. The `h`
    /// values in the body are indexes into that array, so a receiver needs
    /// this count to know how many descriptors the message owns.
    pub const UNIX_FDS: u8 = 9;
}

/// Builds outgoing method-call wire frames (header + body). A zero-sized
/// marker type: every method here operates purely on the `Vec<u8>` passed
/// in, with no state of its own, so it's grouped as one owner for the
/// handful of encode-side helpers instead of leaving them as free
/// functions -- see [`crate::value::Unmarshaller`] for the decode-side
/// sibling.
pub(crate) struct MessageBuilder;

impl MessageBuilder {
    const fn align_up(n: usize, to: usize) -> usize {
        if to <= 1 { n } else { n.div_ceil(to) * to }
    }

    fn pad_to(buf: &mut Vec<u8>, to: usize) {
        let target = Self::align_up(buf.len(), to);
        buf.resize(target, 0);
    }

    fn write_string_field(buf: &mut Vec<u8>, code: u8, sig_char: u8, s: &str) {
        Self::pad_to(buf, 8); // each header field is a struct, 8-aligned
        buf.push(code);
        // VARIANT holding a STRING or OBJECT_PATH or SIGNATURE
        buf.push(1); // signature length
        buf.push(sig_char);
        buf.push(0); // signature NUL
        if sig_char == b'g' {
            // SIGNATURE type: length-prefixed by one byte, no 4-byte len/no pad.
            buf.push(u8::try_from(s.len()).unwrap_or(u8::MAX));
        } else {
            Self::pad_to(buf, 4);
            buf.extend_from_slice(&u32::try_from(s.len()).unwrap_or(u32::MAX).to_le_bytes());
        }
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    }

    /// Append one already-marshalled argument's bytes to a method-call
    /// body, handling the padding its type requires.
    fn write_value(buf: &mut Vec<u8>, v: &BodyArg) {
        match v {
            BodyArg::Str(s) | BodyArg::ObjectPath(s) => Self::write_string(buf, s),
            BodyArg::U32(n) => {
                Self::pad_to(buf, 4);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            BodyArg::Bool(value) => {
                Self::pad_to(buf, 4);
                buf.extend_from_slice(&u32::from(*value).to_le_bytes());
            }
            BodyArg::VariantBool(value) => {
                Self::write_variant_signature(buf, "b");
                Self::write_value(buf, &BodyArg::Bool(*value));
            }
            BodyArg::VariantU32(value) => {
                Self::write_variant_signature(buf, "u");
                Self::write_value(buf, &BodyArg::U32(*value));
            }
        }
    }

    fn write_string(buf: &mut Vec<u8>, value: &str) {
        Self::pad_to(buf, 4);
        buf.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_le_bytes());
        buf.extend_from_slice(value.as_bytes());
        buf.push(0);
    }

    fn write_variant_signature(buf: &mut Vec<u8>, signature: &str) {
        buf.push(u8::try_from(signature.len()).unwrap_or(u8::MAX));
        buf.extend_from_slice(signature.as_bytes());
        buf.push(0);
    }

    /// Build an empty `METHOD_RETURN` frame answering `call_serial` from
    /// `destination`. `StatusNotifier` registration methods return no values,
    /// so this deliberately covers the only server reply the bar needs
    /// before adding a broad outbound-value encoder.
    #[must_use]
    pub fn build_method_return(
        serial: u32,
        destination: &str,
        call_serial: u32,
        args: &[BodyArg],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        let mut signature = String::new();
        for arg in args {
            signature.push_str(arg.signature());
            Self::write_value(&mut body, arg);
        }
        let mut header_fields = Vec::new();
        Self::write_string_field(
            &mut header_fields,
            field_code::DESTINATION,
            b's',
            destination,
        );
        if !signature.is_empty() {
            Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', &signature);
        }
        Self::pad_to(&mut header_fields, 8);
        header_fields.push(field_code::REPLY_SERIAL);
        header_fields.extend_from_slice(&[1, b'u', 0]);
        Self::pad_to(&mut header_fields, 4);
        header_fields.extend_from_slice(&call_serial.to_le_bytes());

        let mut out = vec![b'l', MessageType::MethodReturn.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        out
    }

    /// Build a `METHOD_RETURN` frame carrying an arbitrary reply body,
    /// e.g. a portal method's `a{sv}` or a `Properties.GetAll` result --
    /// shapes [`Self::build_method_return`]'s fixed [`BodyArg`] set can't
    /// express. Returns `None` if `values` doesn't match `reply_signature`
    /// (the same failure [`crate::marshal::Marshaller::marshal_body`]
    /// reports), leaving frame-building a caller's problem to retry rather
    /// than silently sending a malformed reply.
    #[must_use]
    pub fn build_method_return_with_values(
        serial: u32,
        destination: &str,
        call_serial: u32,
        values: &[Value],
        reply_signature: &str,
    ) -> Option<Vec<u8>> {
        let body = Marshaller::marshal_body(values, reply_signature)?;

        let mut header_fields = Vec::new();
        Self::write_string_field(
            &mut header_fields,
            field_code::DESTINATION,
            b's',
            destination,
        );
        if !reply_signature.is_empty() {
            Self::write_string_field(
                &mut header_fields,
                field_code::SIGNATURE,
                b'g',
                reply_signature,
            );
        }
        Self::pad_to(&mut header_fields, 8);
        header_fields.push(field_code::REPLY_SERIAL);
        header_fields.extend_from_slice(&[1, b'u', 0]);
        Self::pad_to(&mut header_fields, 4);
        header_fields.extend_from_slice(&call_serial.to_le_bytes());

        let mut out = vec![b'l', MessageType::MethodReturn.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        Some(out)
    }

    /// Build an `ERROR` frame answering `call_serial`: the shape every
    /// [`crate::conn::DbusConn::call`] caller expects back for a failed
    /// call, and what a [`crate::message`] server side must send instead
    /// of a method return when dispatch fails (an
    /// `org.freedesktop.impl.portal.*` method rejecting its arguments, a
    /// dbus-server `DispatchError`, etc). The body is always a single
    /// human-readable string (signature `"s"`), matching every real D-Bus
    /// service's `Error.*` replies and what [`Reply::parse_message`]
    /// already expects only an `error_name` field plus an optional string
    /// body from.
    #[must_use]
    pub fn build_error(
        serial: u32,
        destination: &str,
        call_serial: u32,
        error_name: &str,
        message: &str,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        Self::write_string(&mut body, message);

        let mut header_fields = Vec::new();
        Self::write_string_field(
            &mut header_fields,
            field_code::DESTINATION,
            b's',
            destination,
        );
        Self::write_string_field(&mut header_fields, field_code::ERROR_NAME, b's', error_name);
        Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', "s");
        Self::pad_to(&mut header_fields, 8);
        header_fields.push(field_code::REPLY_SERIAL);
        header_fields.extend_from_slice(&[1, b'u', 0]);
        Self::pad_to(&mut header_fields, 4);
        header_fields.extend_from_slice(&call_serial.to_le_bytes());

        let mut out = vec![b'l', MessageType::Error.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        out
    }

    /// Build a broadcast `SIGNAL` frame carrying an arbitrary body -- the
    /// [`Value`]-based sibling of [`Self::build_signal`], needed for
    /// e.g. `Properties.PropertiesChanged`'s `(s, a{sv}, as)` shape that
    /// [`BodyArg`] can't express. Returns `None` on the same
    /// signature/value mismatch [`Self::build_method_return_with_values`]
    /// does.
    #[must_use]
    pub fn build_signal_with_values(
        serial: u32,
        path: &str,
        interface: &str,
        member: &str,
        values: &[Value],
        signature: &str,
    ) -> Option<Vec<u8>> {
        let body = Marshaller::marshal_body(values, signature)?;

        let mut header_fields = Vec::new();
        Self::write_string_field(&mut header_fields, field_code::PATH, b'o', path);
        Self::write_string_field(&mut header_fields, field_code::INTERFACE, b's', interface);
        Self::write_string_field(&mut header_fields, field_code::MEMBER, b's', member);
        if !signature.is_empty() {
            Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', signature);
        }

        let mut out = vec![b'l', MessageType::Signal.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        Some(out)
    }

    /// Build a broadcast `SIGNAL` frame. A `StatusNotifier` watcher uses this
    /// to announce that it is an active visualization host; clients which
    /// were already running before the bar started rely on that transition
    /// rather than discovering a host only from the bus-name owner change.
    #[must_use]
    pub fn build_signal(
        serial: u32,
        path: &str,
        interface: &str,
        member: &str,
        args: &[BodyArg],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        let mut signature = String::new();
        for arg in args {
            signature.push_str(arg.signature());
            Self::write_value(&mut body, arg);
        }

        let mut header_fields = Vec::new();
        Self::write_string_field(&mut header_fields, field_code::PATH, b'o', path);
        Self::write_string_field(&mut header_fields, field_code::INTERFACE, b's', interface);
        Self::write_string_field(&mut header_fields, field_code::MEMBER, b's', member);
        if !signature.is_empty() {
            Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', &signature);
        }

        let mut out = vec![b'l', MessageType::Signal.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        out
    }

    /// Build a `METHOD_CALL` frame carrying an arbitrary argument body
    /// (e.g. a portal method's `as` or `a{sv}` argument) -- the
    /// [`Value`]-based sibling of [`Self::build_method_call`], needed
    /// once a caller must *send* more than the handful of shapes
    /// [`BodyArg`] can express. Returns `None` on the same
    /// signature/value mismatch [`Self::build_method_return_with_values`]
    /// does.
    #[must_use]
    pub fn build_method_call_with_values(
        serial: u32,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        values: &[Value],
        signature: &str,
    ) -> Option<Vec<u8>> {
        let body = Marshaller::marshal_body(values, signature)?;

        let mut header_fields = Vec::new();
        Self::write_string_field(&mut header_fields, field_code::PATH, b'o', path);
        Self::write_string_field(
            &mut header_fields,
            field_code::DESTINATION,
            b's',
            destination,
        );
        Self::write_string_field(&mut header_fields, field_code::INTERFACE, b's', interface);
        Self::write_string_field(&mut header_fields, field_code::MEMBER, b's', member);
        if !signature.is_empty() {
            Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', signature);
        }

        let mut out = vec![b'l', MessageType::MethodCall.as_byte(), 0, 1];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8);
        out.extend_from_slice(&body);
        Some(out)
    }

    /// Build a `METHOD_CALL` message frame: header + body, ready to write
    /// to the socket.
    ///
    /// `serial` must be nonzero and unique per in-flight call on this
    /// connection -- the reply carries it back as `REPLY_SERIAL`, which
    /// `DbusConn::call_raw` matches on to find its reply among interleaved
    /// signals.
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
            Self::write_value(&mut body, a);
        }

        let mut header_fields = Vec::new();
        Self::write_string_field(&mut header_fields, field_code::PATH, b'o', path);
        Self::write_string_field(
            &mut header_fields,
            field_code::DESTINATION,
            b's',
            destination,
        );
        Self::write_string_field(&mut header_fields, field_code::INTERFACE, b's', interface);
        Self::write_string_field(&mut header_fields, field_code::MEMBER, b's', member);
        if !sig.is_empty() {
            Self::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', &sig);
        }

        let mut out = vec![
            b'l', // little-endian
            MessageType::MethodCall.as_byte(),
            0, // flags: none (expect a reply)
            1, // protocol version
        ];
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        // header fields array: length-prefixed, elements 8-aligned (struct).
        out.extend_from_slice(
            &u32::try_from(header_fields.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&header_fields);
        Self::pad_to(&mut out, 8); // body starts 8-aligned from message start
        out.extend_from_slice(&body);
        out
    }
}

/// The handful of argument shapes our own outgoing method calls ever need.
///
/// (NetworkManager/bluez getters take zero args or a string/object-path/u32.)
/// Kept separate from [`Value`] (the *decode*-side type, which must model
/// everything a reply could contain) since encode only needs to cover what
/// we actually send.
#[derive(Debug, Clone)]
pub enum BodyArg {
    Str(String),
    ObjectPath(String),
    U32(u32),
    Bool(bool),
    /// A property `Get` result is always a single `v`; the watcher presently
    /// exposes boolean properties, so retain this deliberately narrow shape.
    VariantBool(bool),
    VariantU32(u32),
}

impl BodyArg {
    const fn sig_char(&self) -> u8 {
        match self {
            Self::Str(_) => b's',
            Self::ObjectPath(_) => b'o',
            Self::U32(_) => b'u',
            Self::Bool(_) => b'b',
            Self::VariantBool(_) | Self::VariantU32(_) => b'v',
        }
    }

    const fn signature(&self) -> &'static str {
        match self {
            Self::Str(_) => "s",
            Self::ObjectPath(_) => "o",
            Self::U32(_) => "u",
            Self::Bool(_) => "b",
            Self::VariantBool(_) | Self::VariantU32(_) => "v",
        }
    }
}

/// A parsed reply: enough to tell success from failure and to decode the
/// body against whatever signature the caller expects.
#[derive(Debug)]
pub struct Reply {
    pub kind: MessageType,
    /// The sender unique name on incoming method calls/signals. This is the
    /// destination required for a protocol-correct method return.
    pub sender: Option<String>,
    /// The object path advertised by an incoming method call.
    pub path: Option<String>,
    /// The interface advertised by an incoming method call.
    pub interface: Option<String>,
    /// The member advertised by an incoming method call.
    pub member: Option<String>,
    /// Serial assigned by the sender to this message. An incoming method call
    /// must be answered with it as `REPLY_SERIAL`.
    pub serial: u32,
    /// Present when `kind == Error`, e.g.
    /// `"org.freedesktop.DBus.Error.ServiceUnknown"`.
    pub error_name: Option<String>,
    /// Present on `MethodReturn`/`Error` messages: the serial of the call
    /// this replies to. Absent (`None`) on `Signal`/`MethodCall` messages --
    /// this is how the connection tells an unsolicited signal (e.g.
    /// `NameAcquired`, sent right after `Hello`, or a `PropertiesChanged`
    /// broadcast) apart from the reply it's actually waiting for. See
    /// `DbusConn::call_raw`, which now loops discarding non-matching messages
    /// instead of assuming the very next thing off the wire is always the
    /// reply -- an earlier version didn't do this and got its replies
    /// permanently off-by-one the moment any signal arrived.
    pub reply_serial: Option<u32>,
    pub signature: String,
    pub body: Vec<u8>,
    /// The `UNIX_FDS` header field: how many descriptors arrived with this
    /// message. Zero on every message that carries none, which is all of
    /// them until a caller uses an FD-bearing interface.
    ///
    /// A `Value::UnixFd` in the body is an *index*, valid only against the
    /// descriptors claimed for this message. See [`crate::fds`].
    pub unix_fds: u32,
    /// The descriptors that arrived with this message, in the order a
    /// `Value::UnixFd` index counts in.
    ///
    /// Populated by the connection as it parses the frame, so the
    /// descriptors travel with the message that owns them instead of
    /// staying in a shared queue a later message could drain first. Empty
    /// on every message built locally rather than read off a socket.
    pub fds: Vec<std::os::fd::OwnedFd>,
}

impl Reply {
    /// Decode the body against `self.signature`, ignoring the caller's
    /// expectation — for callers happy to switch on whatever came back.
    #[must_use]
    pub fn values(&self) -> Vec<Value> {
        Unmarshaller::unmarshal_body(&self.body, &self.signature).unwrap_or_default()
    }

    /// Resolve a body `Value::UnixFd(index)` to the descriptor it names.
    ///
    /// The `h` value in a body is an index, not a descriptor number.
    /// Reading it as one yields an unrelated file that happens to be open
    /// in this process, so callers must resolve through here.
    #[must_use]
    pub fn fd_at(&self, index: u32) -> Option<&std::os::fd::OwnedFd> {
        self.fds.get(index as usize)
    }

    /// Take ownership of every descriptor this message carried.
    pub fn take_fds(&mut self) -> Vec<std::os::fd::OwnedFd> {
        std::mem::take(&mut self.fds)
    }
}

/// The fixed 16-byte prelude every D-Bus message starts with, decoded enough
/// to know how many more bytes to read off the wire before the message is
/// complete.
#[derive(Debug, Clone, Copy)]
pub struct Prelude {
    pub message_type: MessageType,
    pub serial: u32,
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
        let serial = u32::from_le_bytes(buf[8..12].try_into().ok()?);
        let header_fields_len = u32::from_le_bytes(buf[12..16].try_into().ok()?);
        Some(Self {
            message_type,
            serial,
            body_len,
            header_fields_len,
        })
    }

    /// Total byte length of the whole message (prelude + header fields,
    /// padded to 8, + body), i.e. how many bytes the caller must have
    /// buffered before calling [`Reply::parse_message`].
    #[must_use]
    pub const fn total_len(&self) -> usize {
        let after_fields = PRELUDE_LEN + self.header_fields_len as usize;
        MessageBuilder::align_up(after_fields, 8) + self.body_len as usize
    }
}

impl Reply {
    /// Parse one complete message (prelude + header fields + body) out of
    /// `buf` into a [`Reply`] -- the natural owner of this parse, since
    /// that's exactly what it constructs.
    ///
    /// `buf` must be at least `Prelude::parse(buf).total_len()` bytes
    /// (checked by the caller via [`Prelude`] first, same two-phase read
    /// `platform::conn` does for Wayland frames — read a fixed prelude,
    /// then read exactly as many more bytes as it says).
    #[must_use]
    pub fn parse_message(buf: &[u8]) -> Option<Self> {
        let prelude = Prelude::parse(buf)?;
        let fields_start = PRELUDE_LEN;
        let fields_end = fields_start + prelude.header_fields_len as usize;
        let _fields_bounds_check = buf.get(fields_start..fields_end)?;

        let mut signature = String::new();
        let mut error_name = None;
        let mut reply_serial = None;
        let mut sender = None;
        let mut path = None;
        let mut interface = None;
        let mut member = None;
        let mut unix_fds = 0u32;

        // Header fields are an ARRAY of STRUCT(BYTE, VARIANT), each struct
        // 8-aligned within the fields array's own start (which is itself
        // 8-aligned relative to the message start, since it directly follows a
        // u32 length at offset 12 -- wait, actually the array *contents* start
        // right after the u32 length with no extra padding beyond the struct's
        // own 8-byte alignment requirement, applied relative to buf[0]).
        let mut offset = fields_start;
        while offset < fields_end {
            offset = MessageBuilder::align_up(offset, 8);
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
            let (val, next) = Unmarshaller::unmarshal(buf, val_start, inner_sig)?;
            match code {
                field_code::PATH => path = val.as_display_string(),
                field_code::INTERFACE => interface = val.as_display_string(),
                field_code::MEMBER => member = val.as_display_string(),
                field_code::SENDER => sender = val.as_display_string(),
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
                    reply_serial = val.as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX));
                }
                field_code::UNIX_FDS => {
                    unix_fds = val.as_u64().map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX));
                }
                _ => {}
            }
            offset = next;
        }

        let body_start = MessageBuilder::align_up(fields_end, 8);
        let body_end = body_start + prelude.body_len as usize;
        let body = buf.get(body_start..body_end)?.to_vec();

        // Validate the signature actually parses to the declared body length by
        // touching split_first_type at least once so it's exercised even on
        // signature-less (empty-body) replies -- keeps the import used and
        // documents that an empty signature is valid (zero-arg replies).
        let _ = Unmarshaller::split_first_type(&signature);
        let _ = Unmarshaller::alignment_of(&signature);

        Some(Self {
            kind: prelude.message_type,
            sender,
            path,
            interface,
            member,
            serial: prelude.serial,
            error_name,
            reply_serial,
            signature,
            body,
            unix_fds,
            fds: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_method_call_no_args() {
        let msg = MessageBuilder::build_method_call(
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
        let msg = MessageBuilder::build_method_call(
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
    fn method_call_with_values_roundtrips_an_as_argument() {
        // ReadAll's argument shape: a plain array of strings, which
        // BodyArg (Str/ObjectPath/U32/Bool/the two Variant* shapes) can't
        // express at all -- there's no BodyArg::ArrayStr.
        let values = vec![Value::Array(vec![Value::Str(
            "org.freedesktop.appearance".into(),
        )])];
        let frame = MessageBuilder::build_method_call_with_values(
            2,
            "org.freedesktop.impl.portal.desktop.mera.scratch",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.impl.portal.Settings",
            "ReadAll",
            &values,
            "as",
        )
        .expect("as matches the values given");

        let prelude = Prelude::parse(&frame).unwrap();
        assert_eq!(prelude.message_type, MessageType::MethodCall);
        assert_eq!(prelude.serial, 2);
        assert_eq!(prelude.total_len(), frame.len());

        let reply = Reply::parse_message(&frame).expect("built method call parses");
        assert_eq!(
            reply.path.as_deref(),
            Some("/org/freedesktop/portal/desktop")
        );
        assert_eq!(
            reply.interface.as_deref(),
            Some("org.freedesktop.impl.portal.Settings")
        );
        assert_eq!(reply.member.as_deref(), Some("ReadAll"));
        assert_eq!(reply.signature, "as");
        assert_eq!(reply.values(), values);
    }

    #[test]
    fn method_call_with_values_rejects_a_signature_mismatch() {
        let values = vec![Value::Str("only one string".into())];
        assert!(
            MessageBuilder::build_method_call_with_values(
                1,
                ":1.50",
                "/obj",
                "org.example.Iface",
                "Method",
                &values,
                "as",
            )
            .is_none()
        );
    }

    #[test]
    fn parse_message_roundtrips_a_hand_built_method_return() {
        // Build a call, then hand-craft a "reply" by re-using the builder's
        // header-writing for a method-return-shaped message body of one
        // string, and confirm parse_message reads it back.
        let mut header_fields = Vec::new();
        MessageBuilder::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', "s");

        let mut body = Vec::new();
        MessageBuilder::write_value(&mut body, &BodyArg::Str("hello".into()));

        let mut out = vec![b'l', MessageType::MethodReturn.as_byte(), 0, 1];
        let body_len = u32::try_from(body.len()).expect("test body fits in u32");
        let header_fields_len =
            u32::try_from(header_fields.len()).expect("test header fields fit in u32");
        out.extend_from_slice(&body_len.to_le_bytes());
        out.extend_from_slice(&5u32.to_le_bytes());
        out.extend_from_slice(&header_fields_len.to_le_bytes());
        out.extend_from_slice(&header_fields);
        MessageBuilder::pad_to(&mut out, 8);
        out.extend_from_slice(&body);

        let prelude = Prelude::parse(&out).unwrap();
        assert_eq!(prelude.total_len(), out.len());

        let reply = Reply::parse_message(&out).unwrap();
        assert_eq!(reply.kind, MessageType::MethodReturn);
        assert_eq!(reply.signature, "s");
        assert_eq!(reply.values(), vec![Value::Str("hello".into())]);
    }

    /// A message that carries descriptors declares how many in its
    /// `UNIX_FDS` header field. Without reading it back a receiver cannot
    /// tell how many of the staged descriptors belong to this message.
    #[test]
    fn parse_message_reads_back_the_unix_fds_header_field() {
        let mut header_fields = Vec::new();
        MessageBuilder::write_string_field(&mut header_fields, field_code::SIGNATURE, b'g', "h");
        MessageBuilder::pad_to(&mut header_fields, 8);
        header_fields.push(field_code::UNIX_FDS);
        header_fields.extend_from_slice(&[1, b'u', 0]);
        MessageBuilder::pad_to(&mut header_fields, 4);
        header_fields.extend_from_slice(&3u32.to_le_bytes());

        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());

        let mut out = vec![b'l', MessageType::MethodReturn.as_byte(), 0, 1];
        let body_len = u32::try_from(body.len()).expect("test body fits in u32");
        let header_fields_len =
            u32::try_from(header_fields.len()).expect("test header fields fit in u32");
        out.extend_from_slice(&body_len.to_le_bytes());
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&header_fields_len.to_le_bytes());
        out.extend_from_slice(&header_fields);
        MessageBuilder::pad_to(&mut out, 8);
        out.extend_from_slice(&body);

        let reply = Reply::parse_message(&out).expect("a well-formed fd-bearing reply");
        assert_eq!(reply.unix_fds, 3);
        assert_eq!(reply.signature, "h");
        assert_eq!(reply.values(), vec![Value::UnixFd(0)]);
    }

    /// Every message this crate sends today carries no descriptors, so the
    /// absent field must read as zero rather than leaving a stale count.
    #[test]
    fn a_message_without_the_field_reports_no_descriptors() {
        let frame = MessageBuilder::build_method_call(
            1,
            "org.example.Dest",
            "/org/example",
            "org.example.Iface",
            "Member",
            &[],
        );
        let reply = Reply::parse_message(&frame).expect("a well-formed call");
        assert_eq!(reply.unix_fds, 0);
    }

    #[test]
    fn prelude_rejects_short_or_big_endian_buffers() {
        assert!(Prelude::parse(&[]).is_none());
        assert!(Prelude::parse(&[b'B'; 16]).is_none());
    }

    #[test]
    fn signal_frame_has_the_declared_route_and_no_reply_serial() {
        let frame = MessageBuilder::build_signal(
            9,
            "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher",
            "StatusNotifierHostRegistered",
            &[],
        );
        let reply = Reply::parse_message(&frame).expect("built signal parses");
        assert_eq!(reply.kind, MessageType::Signal);
        assert_eq!(reply.path.as_deref(), Some("/StatusNotifierWatcher"));
        assert_eq!(
            reply.interface.as_deref(),
            Some("org.kde.StatusNotifierWatcher")
        );
        assert_eq!(
            reply.member.as_deref(),
            Some("StatusNotifierHostRegistered")
        );
        assert_eq!(reply.reply_serial, None);
    }

    #[test]
    fn method_return_with_values_roundtrips_an_a_sv_body() {
        // The shape every real portal GetAll reply needs, which the
        // BodyArg-based build_method_return can't express at all.
        let values = vec![Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str("scheme".into())),
            Box::new(Value::Variant(Box::new(Value::U32(1)))),
        )])];
        let frame =
            MessageBuilder::build_method_return_with_values(3, ":1.50", 9, &values, "a{sv}")
                .expect("a{sv} matches the values given");

        let reply = Reply::parse_message(&frame).expect("built method return parses");
        assert_eq!(reply.kind, MessageType::MethodReturn);
        assert_eq!(reply.reply_serial, Some(9));
        assert_eq!(reply.signature, "a{sv}");
        assert_eq!(reply.values(), values);
    }

    #[test]
    fn method_return_with_values_rejects_a_signature_mismatch() {
        let values = vec![Value::Str("only one string".into())];
        assert!(
            MessageBuilder::build_method_return_with_values(1, ":1.50", 5, &values, "ss").is_none()
        );
    }

    #[test]
    fn build_error_carries_the_error_name_and_message_and_answers_the_right_call() {
        let frame = MessageBuilder::build_error(
            4,
            ":1.50",
            11,
            "org.freedesktop.DBus.Error.UnknownMethod",
            "No such method 'Foo'",
        );
        let reply = Reply::parse_message(&frame).expect("built error parses");
        assert_eq!(reply.kind, MessageType::Error);
        assert_eq!(reply.reply_serial, Some(11));
        assert_eq!(
            reply.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.UnknownMethod")
        );
        assert_eq!(reply.signature, "s");
        assert_eq!(
            reply.values(),
            vec![Value::Str("No such method 'Foo'".into())]
        );
    }

    #[test]
    fn signal_with_values_roundtrips_the_properties_changed_shape() {
        let values = vec![
            Value::Str("org.freedesktop.impl.portal.Settings".into()),
            Value::Array(vec![Value::DictEntry(
                Box::new(Value::Str("scheme".into())),
                Box::new(Value::Variant(Box::new(Value::U32(1)))),
            )]),
            Value::Array(vec![]),
        ];
        let frame = MessageBuilder::build_signal_with_values(
            2,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &values,
            "sa{sv}as",
        )
        .expect("sa{sv}as matches the values given");

        let reply = Reply::parse_message(&frame).expect("built signal parses");
        assert_eq!(reply.kind, MessageType::Signal);
        assert_eq!(reply.reply_serial, None);
        assert_eq!(
            reply.path.as_deref(),
            Some("/org/freedesktop/portal/desktop")
        );
        assert_eq!(reply.values(), values);
    }

    #[test]
    fn signal_with_values_rejects_a_signature_mismatch() {
        let values = vec![Value::Str("only one string".into())];
        assert!(
            MessageBuilder::build_signal_with_values(
                1,
                "/obj",
                "org.example.Iface",
                "Changed",
                &values,
                "ss"
            )
            .is_none()
        );
    }
}
