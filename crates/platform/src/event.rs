//! The event type that crosses the reader→main channel.
//!
//! The reader thread is a pure parser: it pulls bytes off the socket, splits
//! them into whole Wayland messages, and pushes each one as a [`RawEvent`] onto
//! a lock-free SPSC ring. It does **not** interpret meaning — it doesn't know
//! which object id is the pointer or the surface, because those ids are
//! allocated on the main thread. The main thread, which owns id allocation, maps
//! each event to meaning.
//!
//! Most Wayland events we care about carry a small, fixed set of `u32`/`i32`
//! arguments (pointer motion, configure, ping — all ≤ 4 words), so the common
//! [`RawEvent::Wire`] variant inlines them with no allocation. The few
//! variable-length or string-bearing events (`wl_display.error`,
//! `wl_registry.global`) are rarer and carried as raw bytes in
//! [`RawEvent::Bytes`], parsed on the main thread.
//!
//! The `object_id` and `opcode` are stored raw — the reader can't know which
//! interface an id belongs to, so it can't produce an interface-typed
//! [`ObjectId`](crate::id::ObjectId). The main thread, which *does* know, uses
//! the typed accessors below ([`is`](RawEvent::is), [`event`](RawEvent::event),
//! [`serial`](RawEvent::serial), [`fixed`](RawEvent::fixed), [`point`](RawEvent::point))
//! to interpret each event without sprinkling `.raw()` / `Serial(..)` / `Fixed(..)`
//! conversions through the dispatch code.
//!
//! This is the middle link of three unrelated event types the input pipeline
//! threads through, hence the specific name: [`crate::wire::WireEvent`] is
//! this one's raw material (freshly parsed off the socket, still just a
//! header + byte cursor), and `vt_parser::TermEvent` is downstream of the
//! PTY entirely, several stages later. Calling all three bare `Event` used
//! to force every call site to alias-import to tell them apart.

use crate::draw::Point;
use crate::id::{ObjectId, Serial};
use crate::protocol::{Fixed, FromOpcode};

/// One decoded Wayland message handed from the reader thread to the main thread.
///
/// Not `Clone`: the `Fd` variant owns `OwnedFd`s, which can't be duplicated.
#[derive(Debug)]
pub enum RawEvent {
    /// A message whose arguments fit in up to four 32-bit words. `args[..nargs]`
    /// are valid; interpret them per `(object_id, opcode)`. Pointer motion,
    /// configure, ping, seat capabilities, toplevel close all land here.
    Wire {
        object_id: u32,
        opcode: u16,
        args: [u32; 4],
        nargs: u8,
        /// Full encoded bytes retained for known string-bearing events from
        /// non-singleton objects such as `wl_output.name`.
        bytes: Box<[u8]>,
    },
    /// A message that doesn't fit the fixed-arg shape (strings, >4 words). The
    /// main thread re-parses the raw message bytes. Used for `wl_display.error`
    /// and `wl_registry.global`.
    Bytes(Box<[u8]>),
    /// Not a Wayland message at all: the key-repeat timerfd expired. The reader
    /// thread polls that fd alongside the socket and emits this tick so the main
    /// thread — which owns the held-key state — can re-apply the held key's
    /// motion. Carries no payload; the main thread already knows what's held.
    Repeat,
    /// File descriptors received as ancillary (`SCM_RIGHTS`) data alongside a
    /// message. Wayland attaches fds *out of band* to a specific message
    /// (`wl_data_source.send`, `wl_data_offer.receive`, `wl_keyboard.keymap`),
    /// not as a standalone message — so the reader forwards the message itself
    /// (as `Wire`/`Bytes`) and, separately, the batch of fds that arrived with
    /// the same recv completion, as one `Fd` event pushed immediately after.
    /// The main thread correlates them by order: the `Fd` batch belongs to the
    /// message event that just preceded it.
    Fd(Vec<std::os::fd::OwnedFd>),
}

impl RawEvent {
    /// Decode one whole wire message (`msg`, including its 8-byte header)
    /// into a `RawEvent`. Was a free `encode(msg: &[u8]) -> Event` function in
    /// `conn.rs`'s reader thread -- it builds nothing but a `RawEvent`, so it
    /// belongs as a constructor on the type it returns.
    ///
    /// Messages from `wl_display` and `wl_registry` carry strings, so
    /// they're forwarded as raw bytes ([`RawEvent::Bytes`]) for the main thread
    /// to re-parse with the full wire decoder. Everything else inlines up to
    /// four `u32` args into [`RawEvent::Wire`].
    ///
    /// # Panics
    /// If `msg` is shorter than the 8-byte Wayland message header.
    #[must_use]
    pub fn from_wire_msg(msg: &[u8]) -> Self {
        let object_id = u32::from_ne_bytes(
            msg[0..4]
                .try_into()
                .expect("Wayland message shorter than 4-byte header"),
        );
        let opcode = u16::from_ne_bytes(
            msg[4..6]
                .try_into()
                .expect("Wayland message shorter than 6-byte header"),
        );

        if object_id == crate::conn::DISPLAY_ID || object_id == crate::conn::REGISTRY_ID {
            return Self::Bytes(msg.to_vec().into_boxed_slice());
        }

        // The reader deliberately has no object-interface map. Keep the full
        // wire message for the main thread, which *does* know whether a given
        // event is string-bearing; guessing from the first argument can
        // misclassify perfectly ordinary numeric protocol events.
        let body = &msg[8..];

        // Read up to four u32 args from the message body (after the header).
        let mut args = [0u32; 4];
        let n = (body.len() / 4).min(4);
        for (i, slot) in args.iter_mut().enumerate().take(n) {
            let off = i * 4;
            *slot = u32::from_ne_bytes(
                body[off..off + 4]
                    .try_into()
                    .expect("Wayland message body shorter than argument window"),
            );
        }
        Self::Wire {
            object_id,
            opcode,
            args,
            // `n <= 4` from the `.min(4)` above, always fits a `u8`.
            nargs: u8::try_from(n).unwrap_or(4),
            bytes: msg.to_vec().into_boxed_slice(),
        }
    }

    /// The object id this event is about (raw wire value), or 0 for the
    /// non-`Wire` variants (`Bytes`, `Repeat`), which aren't about an object.
    #[must_use]
    pub const fn object_id(&self) -> u32 {
        match self {
            Self::Wire { object_id, .. } => *object_id,
            Self::Bytes(_) | Self::Repeat | Self::Fd(_) => 0,
        }
    }

    /// The opcode of this event (raw wire value), or 0 for the non-`Wire`
    /// variants (`Bytes`, `Repeat`).
    #[must_use]
    pub const fn opcode(&self) -> u16 {
        match self {
            Self::Wire { opcode, .. } => *opcode,
            Self::Bytes(_) | Self::Repeat | Self::Fd(_) => 0,
        }
    }

    /// Whether this event is about object `id` — the typed way to match an
    /// incoming event against one of our objects: `ev.is(self.surface)`.
    pub fn is(&self, id: impl ObjectId) -> bool {
        self.object_id() == id.raw()
    }

    /// Decode this event's opcode into the event enum `E`, or `None` if it isn't
    /// one `E` models: `ev.event::<PointerEvent>()`.
    #[must_use]
    pub fn event<E: FromOpcode>(&self) -> Option<E> {
        E::from_opcode(self.opcode())
    }

    /// Full encoded message for a [`Self::Wire`] event, including its header.
    #[must_use]
    pub fn wire_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Wire { bytes, .. } => Some(bytes),
            Self::Bytes(_) | Self::Repeat | Self::Fd(_) => None,
        }
    }

    /// Read fixed arg `i` as a `u32` (0 if out of range).
    #[must_use]
    pub const fn arg(&self, i: usize) -> u32 {
        match self {
            Self::Wire { args, nargs, .. } if i < *nargs as usize => args[i],
            _ => 0,
        }
    }

    /// Read fixed arg `i` reinterpreted as `i32` (for `wl_fixed`/signed args).
    #[must_use]
    pub const fn arg_i32(&self, i: usize) -> i32 {
        self.arg(i).cast_signed()
    }

    /// Read arg `i` as an event [`Serial`].
    #[must_use]
    pub const fn serial(&self, i: usize) -> Serial {
        Serial(self.arg(i))
    }

    /// Read arg `i` as a [`Fixed`] (24.8 fixed-point) coordinate.
    #[must_use]
    pub const fn fixed(&self, i: usize) -> Fixed {
        Fixed(self.arg_i32(i))
    }

    /// Read args `ix` and `iy` as a [`Point`], converting from `wl_fixed`.
    #[must_use]
    pub const fn point(&self, ix: usize, iy: usize) -> Point {
        Point::new(self.fixed(ix).to_pixels(), self.fixed(iy).to_pixels())
    }
}
