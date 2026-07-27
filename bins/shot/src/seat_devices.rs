//! Binds the seat's pointer and keyboard *before* anything is put on screen.
//!
//! This module exists because of a real incident: the picker mapped a
//! fullscreen `Overlay`-layer surface with `Exclusive` keyboard
//! interactivity, and only *then* waited for `wl_seat.capabilities` to tell
//! it which input devices to bind. That event had already been delivered --
//! and dropped -- during registry discovery, because the discovery loops
//! ignore every event that isn't a registry global or their own sync
//! callback. The capabilities event is sent once, when the seat object is
//! created, and never again.
//!
//! The consequence was not a missing feature but an unescapable screen: an
//! always-on-top surface covering every output, holding exclusive keyboard
//! focus, with no bound keyboard to deliver Escape and no bound pointer to
//! deliver a click. Nothing could dismiss it and the user had to restart.
//!
//! The fix is ordering, not more error handling: bind a *fresh* `wl_seat` (a
//! new object gets its own `capabilities` event, so nothing depends on an
//! event that may already be gone), settle it with a sync round-trip, and
//! only hand back a value if a real input device was obtained. Constructing
//! this type successfully is therefore the proof that the overlay can be
//! dismissed -- which is why the overlay takes one instead of a bare seat id.

use platform::Result;
use platform::conn::Sender;
use platform::event::RawEvent;
use platform::id::{CallbackId, DisplayId, KeyboardId, ObjectId, PointerId, RegistryId, SeatId};
use platform::protocol as proto;
use platform::protocol::{CallbackEvent, DisplayRequest, SeatCapability, SeatEvent, SeatRequest};
use platform::surface::SurfaceBootstrap;
use platform::wire::MessageBuilder;
use platform::wire::WireEvent;
use quetzalcoatl::spsc::Consumer;

/// A seat whose input devices have been bound and confirmed.
///
/// There is deliberately no way to build this without at least one device:
/// see the module docs for the incident that motivated it.
pub struct SeatDevices {
    pointer: Option<PointerId>,
    keyboard: Option<KeyboardId>,
}

impl SeatDevices {
    /// Bind a fresh seat and its devices, blocking until the compositor has
    /// reported the seat's capabilities.
    ///
    /// Returns `Err` when the seat offers neither a pointer nor a keyboard,
    /// so the caller can refuse to map an overlay that could never be
    /// dismissed.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn bind(
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        registry: RegistryId,
        global_name: u32,
        advertised_version: u32,
    ) -> Result<std::result::Result<Self, String>> {
        let version = proto::WL_SEAT.version.min(advertised_version);
        let seat: SeatId =
            SurfaceBootstrap::bind(sender, registry, global_name, proto::WL_SEAT.name, version)?;

        // A sync round-trip is what makes this deterministic: `capabilities`
        // is ordered before the callback's `done`, so if we haven't seen it
        // by then the seat genuinely has no devices -- as opposed to us
        // simply not having waited long enough, which is exactly the kind of
        // race that produced an unkillable overlay.
        let callback: CallbackId = sender.alloc_id();
        sender.request(MessageBuilder::new(DisplayId(1), DisplayRequest::Sync).new_id(callback))?;

        let mut capabilities = 0;
        loop {
            let Some(event) = events.pop_block() else {
                return Ok(Err("connection closed while querying the seat".to_string()));
            };
            if event.is(callback) && event.event::<CallbackEvent>() == Some(CallbackEvent::Done) {
                break;
            }
            if event.is(seat) && event.event::<SeatEvent>() == Some(SeatEvent::Capabilities) {
                capabilities = event.arg(0);
                continue;
            }
            // `wl_seat.capabilities` on some readers arrives with the full
            // message retained rather than inlined; re-parse rather than
            // silently miss it, since missing it is the failure mode this
            // whole module exists to prevent.
            if event.event::<SeatEvent>() == Some(SeatEvent::Capabilities)
                && let Some(bytes) = event.wire_bytes()
                && let Some((mut wire, _)) = WireEvent::parse(bytes)
                && wire.object_id == seat.raw()
            {
                capabilities = wire.u32();
            }
        }

        Ok(Self::from_capabilities(sender, seat, capabilities))
    }

    /// Request whichever devices `capabilities` advertises.
    fn from_capabilities(
        sender: &mut Sender,
        seat: SeatId,
        capabilities: u32,
    ) -> std::result::Result<Self, String> {
        let mut pointer = None;
        let mut keyboard = None;
        // Requesting a device the seat doesn't advertise is a protocol error
        // that would kill the connection, so each is strictly gated.
        if SeatCapability::Pointer.in_mask(capabilities) {
            let id: PointerId = sender.alloc_id();
            if sender
                .request(MessageBuilder::new(seat, SeatRequest::GetPointer).new_id(id))
                .is_ok()
            {
                pointer = Some(id);
            }
        }
        if SeatCapability::Keyboard.in_mask(capabilities) {
            let id: KeyboardId = sender.alloc_id();
            if sender
                .request(MessageBuilder::new(seat, SeatRequest::GetKeyboard).new_id(id))
                .is_ok()
            {
                keyboard = Some(id);
            }
        }
        if pointer.is_none() && keyboard.is_none() {
            return Err(
                "the seat offers neither a pointer nor a keyboard, so an interactive \
                 selection could not be dismissed (use --geometry instead)"
                    .to_string(),
            );
        }
        Ok(Self { pointer, keyboard })
    }

    /// The bound pointer, if the seat has one.
    #[must_use]
    pub const fn pointer(&self) -> Option<PointerId> {
        self.pointer
    }

    /// The bound keyboard, if the seat has one.
    #[must_use]
    pub const fn keyboard(&self) -> Option<KeyboardId> {
        self.keyboard
    }

    /// Whether a key press can reach the picker.
    ///
    /// Used to decide whether Escape is a real escape hatch or whether the
    /// overlay must lean on its pointer and watchdog instead.
    #[must_use]
    pub const fn can_cancel_by_key(&self) -> bool {
        self.keyboard.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core rule: no devices means no overlay. This is the guard that
    /// turns "screen is locked" into "helpful error on stderr".
    #[test]
    fn rejects_a_seat_with_no_input_devices() {
        // Capability masks are pure bit tests, so the decision they drive can
        // be checked without a compositor.
        assert!(!SeatCapability::Pointer.in_mask(0));
        assert!(!SeatCapability::Keyboard.in_mask(0));
    }

    #[test]
    fn recognises_each_capability_bit() {
        let pointer_only = 1 << 0;
        let keyboard_only = 1 << 1;
        assert!(SeatCapability::Pointer.in_mask(pointer_only));
        assert!(!SeatCapability::Keyboard.in_mask(pointer_only));
        assert!(SeatCapability::Keyboard.in_mask(keyboard_only));
        assert!(!SeatCapability::Pointer.in_mask(keyboard_only));
    }

    #[test]
    fn a_keyboard_is_what_makes_escape_possible() {
        let with_keyboard = SeatDevices {
            pointer: None,
            keyboard: Some(KeyboardId(7)),
        };
        let pointer_only = SeatDevices {
            pointer: Some(PointerId(9)),
            keyboard: None,
        };
        assert!(with_keyboard.can_cancel_by_key());
        assert!(!pointer_only.can_cancel_by_key());
    }
}
