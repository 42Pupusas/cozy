#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss,
         clippy::use_self, clippy::must_use_candidate, clippy::doc_markdown)]
//! The Wayland wire format: how requests and events are encoded as bytes.
//!
//! Every Wayland message, in either direction, is:
//!
//! ```text
//!   u32  object id        (which object the message is about)
//!   u16  opcode           (which request/event of that object)
//!   u16  size             (total message size in bytes, incl. this header)
//!   ...  arguments        (each padded to a 4-byte boundary)
//! ```
//!
//! Integers are host-endian (little, on x86-64). Argument types we handle:
//!   * `u32` / `i32` / `object` / `new_id`  — one word
//!   * `string` — u32 length (incl. trailing NUL), bytes, padded to 4
//!   * `array`  — u32 length, bytes, padded to 4
//!   * `fd`     — carried out-of-band as ancillary data, *not* in the body
//!
//! This module is pure byte-shuffling; no I/O and no unsafe.

use crate::id::ObjectId;
use crate::protocol::Opcode;

/// Builds the body of a single outgoing request.
pub struct MessageBuilder {
    buf: Vec<u8>,
}

impl MessageBuilder {
    /// Begin a `request` on `object`. The request enum carries its own wire
    /// opcode via [`Opcode`], so the call site reads `SurfaceRequest::Commit`
    /// rather than a bare number. The size field is patched in `finish`.
    pub fn new(object: impl ObjectId, request: impl Opcode) -> Self {
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&object.raw().to_ne_bytes());
        // opcode (low u16) + size (high u16); size filled in later.
        buf.extend_from_slice(&request.opcode().to_ne_bytes());
        buf.extend_from_slice(&0u16.to_ne_bytes());
        Self { buf }
    }

    pub fn u32(mut self, v: u32) -> Self {
        self.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub fn i32(mut self, v: i32) -> Self {
        self.buf.extend_from_slice(&v.to_ne_bytes());
        self
    }

    /// An object id argument (a reference to an existing object).
    pub fn object(self, id: impl ObjectId) -> Self {
        self.u32(id.raw())
    }

    /// A `new_id` argument: the id we're assigning to an object we're creating.
    pub fn new_id(self, id: impl ObjectId) -> Self {
        self.u32(id.raw())
    }

    /// A string: length-prefixed, NUL-terminated, padded to 4 bytes.
    pub fn string(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let len_with_nul = bytes.len() + 1;
        self.buf
            .extend_from_slice(&(len_with_nul as u32).to_ne_bytes());
        self.buf.extend_from_slice(bytes);
        self.buf.push(0);
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
        self
    }

    /// Finalize: patch the size field and return the encoded bytes.
    pub fn finish(mut self) -> Vec<u8> {
        let size = self.buf.len() as u16;
        // size occupies bytes 6..8 (the high half of the opcode/size word).
        self.buf[6..8].copy_from_slice(&size.to_ne_bytes());
        self.buf
    }
}

/// A decoded event header plus a cursor over its argument bytes.
pub struct Event<'a> {
    pub object_id: u32,
    pub opcode: u16,
    args: &'a [u8],
    pos: usize,
}

impl<'a> Event<'a> {
    /// Try to parse one event from the front of `data`. Returns the event and
    /// the number of bytes it consumed, or `None` if `data` is a partial
    /// header (caller should read more).
    pub fn parse(data: &'a [u8]) -> Option<(Event<'a>, usize)> {
        if data.len() < 8 {
            return None;
        }
        let object_id = u32::from_ne_bytes(data[0..4].try_into().unwrap());
        let opcode = u16::from_ne_bytes(data[4..6].try_into().unwrap());
        let size = u16::from_ne_bytes(data[6..8].try_into().unwrap()) as usize;
        if size < 8 || data.len() < size {
            return None;
        }
        Some((
            Event {
                object_id,
                opcode,
                args: &data[8..size],
                pos: 0,
            },
            size,
        ))
    }

    pub fn u32(&mut self) -> u32 {
        let v = u32::from_ne_bytes(self.args[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }

    /// Decode a signed arg. Unused in step one but needed for events that carry
    /// dimensions (e.g. xdg_toplevel.configure width/height) in later steps.
    #[allow(dead_code)]
    pub fn i32(&mut self) -> i32 {
        let v = i32::from_ne_bytes(self.args[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }

    /// Read a string argument and return it without the trailing NUL.
    pub fn string(&mut self) -> String {
        let len = self.u32() as usize; // includes NUL
        let start = self.pos;
        let text = &self.args[start..start + len - 1]; // drop NUL
        let s = String::from_utf8_lossy(text).into_owned();
        // advance past content + NUL, padded to 4.
        let padded = (len + 3) & !3;
        self.pos += padded;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{DisplayId, RegistryId, SurfaceId};
    use crate::protocol::{CompositorRequest, DisplayRequest, RegistryRequest};

    #[test]
    fn header_size_is_patched() {
        // wl_display.get_registry(new_id=2) on object 1 (opcode 1).
        let bytes = MessageBuilder::new(DisplayId(1), DisplayRequest::GetRegistry)
            .new_id(RegistryId(2))
            .finish();
        assert_eq!(bytes.len(), 12);
        let size = u16::from_ne_bytes(bytes[6..8].try_into().unwrap());
        assert_eq!(size as usize, bytes.len());
        let oid = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(oid, 1);
        let opcode = u16::from_ne_bytes(bytes[4..6].try_into().unwrap());
        assert_eq!(opcode, DisplayRequest::GetRegistry.opcode());
    }

    #[test]
    fn string_is_nul_terminated_and_padded() {
        // "wl_shm" is 6 bytes; +NUL = 7; padded to 8; +4 len prefix = 12.
        let bytes = MessageBuilder::new(RegistryId(3), RegistryRequest::Bind)
            .string("wl_shm")
            .finish();
        let body = &bytes[8..];
        let len = u32::from_ne_bytes(body[0..4].try_into().unwrap());
        assert_eq!(len, 7);
        assert_eq!(body.len(), 12); // 4 len + 8 padded string
    }

    #[test]
    fn roundtrip_event_parse() {
        // Encode a fake message with a u32 then parse it back.
        let raw = MessageBuilder::new(SurfaceId(42), CompositorRequest::CreateSurface)
            .u32(0xDEAD_BEEF)
            .finish();
        let total = raw.len();
        // MessageBuilder writes a request, but the wire layout is identical to
        // an event, so we can parse it as one.
        let (mut ev, consumed) = Event::parse(&raw).unwrap();
        assert_eq!(consumed, total);
        assert_eq!(ev.object_id, 42);
        assert_eq!(ev.u32(), 0xDEAD_BEEF);
    }
}
