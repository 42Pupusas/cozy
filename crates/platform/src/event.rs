//! The event type that crosses the reader→main channel.
//!
//! The reader thread is a pure parser: it pulls bytes off the socket, splits
//! them into whole Wayland messages, and pushes each one as an [`Event`] onto a
//! lock-free SPSC ring. It does **not** interpret meaning — it doesn't know
//! which object id is the pointer or the surface, because those ids are
//! allocated on the main thread. The main thread, which owns id allocation, maps
//! each event to meaning.
//!
//! Most Wayland events we care about carry a small, fixed set of `u32`/`i32`
//! arguments (pointer motion, configure, ping — all ≤ 4 words), so the common
//! [`Event::Wire`] variant inlines them with no allocation. The few
//! variable-length or string-bearing events (`wl_display.error`,
//! `wl_registry.global`) are rarer and carried as raw bytes in
//! [`Event::Bytes`], parsed on the main thread.
//!
//! The `object_id` and `opcode` are stored raw — the reader can't know which
//! interface an id belongs to, so it can't produce an interface-typed
//! [`ObjectId`](crate::id::ObjectId). The main thread, which *does* know, uses
//! the typed accessors below ([`is`](Event::is), [`event`](Event::event),
//! [`serial`](Event::serial), [`fixed`](Event::fixed), [`point`](Event::point))
//! to interpret each event without sprinkling `.raw()` / `Serial(..)` / `Fixed(..)`
//! conversions through the dispatch code.

use crate::draw::Point;
use crate::id::{ObjectId, Serial};
use crate::protocol::{Fixed, FromOpcode};

/// One decoded Wayland message handed from the reader thread to the main thread.
#[derive(Clone, Debug)]
pub enum Event {
    /// A message whose arguments fit in up to four 32-bit words. `args[..nargs]`
    /// are valid; interpret them per `(object_id, opcode)`. Pointer motion,
    /// configure, ping, seat capabilities, toplevel close all land here.
    Wire {
        object_id: u32,
        opcode: u16,
        args: [u32; 4],
        nargs: u8,
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
}

impl Event {
    /// The object id this event is about (raw wire value), or 0 for the
    /// non-`Wire` variants (`Bytes`, `Repeat`), which aren't about an object.
    pub fn object_id(&self) -> u32 {
        match self {
            Event::Wire { object_id, .. } => *object_id,
            Event::Bytes(_) | Event::Repeat => 0,
        }
    }

    /// The opcode of this event (raw wire value), or 0 for the non-`Wire`
    /// variants (`Bytes`, `Repeat`).
    pub fn opcode(&self) -> u16 {
        match self {
            Event::Wire { opcode, .. } => *opcode,
            Event::Bytes(_) | Event::Repeat => 0,
        }
    }

    /// Whether this event is about object `id` — the typed way to match an
    /// incoming event against one of our objects: `ev.is(self.surface)`.
    pub fn is(&self, id: impl ObjectId) -> bool {
        self.object_id() == id.raw()
    }

    /// Decode this event's opcode into the event enum `E`, or `None` if it isn't
    /// one `E` models: `ev.event::<PointerEvent>()`.
    pub fn event<E: FromOpcode>(&self) -> Option<E> {
        E::from_opcode(self.opcode())
    }

    /// Read fixed arg `i` as a `u32` (0 if out of range).
    pub fn arg(&self, i: usize) -> u32 {
        match self {
            Event::Wire { args, nargs, .. } if i < *nargs as usize => args[i],
            _ => 0,
        }
    }

    /// Read fixed arg `i` reinterpreted as `i32` (for `wl_fixed`/signed args).
    pub fn arg_i32(&self, i: usize) -> i32 {
        self.arg(i) as i32
    }

    /// Read arg `i` as an event [`Serial`].
    pub fn serial(&self, i: usize) -> Serial {
        Serial(self.arg(i))
    }

    /// Read arg `i` as a [`Fixed`] (24.8 fixed-point) coordinate.
    pub fn fixed(&self, i: usize) -> Fixed {
        Fixed(self.arg_i32(i))
    }

    /// Read args `ix` and `iy` as a [`Point`], converting from `wl_fixed`.
    pub fn point(&self, ix: usize, iy: usize) -> Point {
        Point::new(self.fixed(ix).to_pixels(), self.fixed(iy).to_pixels())
    }
}
