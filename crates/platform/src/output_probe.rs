//! Reading an output's logical size without stealing anyone else's events.
//!
//! Probing an output means a `wl_display.sync` round-trip: request the
//! callback, then read events until its `done` proves every `wl_output`
//! property has been delivered. The hazard is what else arrives inside that
//! window. A client that has already bound `wl_seat` will typically receive
//! `wl_seat.capabilities` during the very same burst, and capabilities is
//! sent exactly once -- a probe loop that reads an event, finds it
//! uninteresting, and drops it silently costs the client its keyboard and
//! pointer for the rest of the process's life.
//!
//! So this probe keeps what it does not consume. Events that belong to the
//! caller are moved into a spill buffer the caller replays into its normal
//! event handler, making the probe transparent to everything but the output
//! properties it was asked to read.

use crate::conn::Sender;
use crate::event::RawEvent;
use crate::id::{CallbackId, DisplayId, ObjectId, OutputId};
use crate::protocol::{CallbackEvent, DisplayRequest, OutputEvent};
use crate::wire::{MessageBuilder, WireEvent};
use quetzalcoatl::spsc::Consumer;

/// One output-property round-trip plus the events it had to read past.
///
/// Holds the spill so the borrow of the event ring ends with the probe:
/// callers drain [`Self::take_spill`] into their own dispatch afterwards.
#[derive(Debug, Default)]
pub struct OutputProbe {
    spill: Vec<RawEvent>,
    logical: Option<(i32, i32)>,
    scale: i32,
}

impl OutputProbe {
    /// `wl_output.mode` flag marking the mode the output is actually driving.
    const MODE_CURRENT: u32 = 1;

    /// Run the round-trip against `output`, collecting its current mode and
    /// scale and retaining every unrelated event for the caller.
    ///
    /// Returns a probe whose [`Self::logical_size`] is `None` when the output
    /// never reported a usable mode (or the connection ended first), so
    /// callers fall back rather than guess.
    #[must_use]
    pub fn read(sender: &mut Sender, events: &mut Consumer<RawEvent>, output: OutputId) -> Self {
        let mut probe = Self {
            spill: Vec::new(),
            logical: None,
            scale: 1,
        };
        let cb: CallbackId = sender.alloc_id();
        if sender
            .request(MessageBuilder::new(DisplayId(1), DisplayRequest::Sync).new_id(cb))
            .is_err()
        {
            return probe;
        }

        let mut phys = None;
        loop {
            let Some(event) = events.pop_block() else {
                break;
            };
            if probe.absorb(event, output, cb, &mut phys) {
                break;
            }
        }
        probe.logical = phys.map(|(w, h)| (w / probe.scale, h / probe.scale));
        probe
    }

    /// Consume one event: record output properties, report the terminating
    /// callback, and spill anything that belongs to the caller.
    ///
    /// Returns `true` when `event` was the sync callback's `done`.
    fn absorb(
        &mut self,
        event: RawEvent,
        output: OutputId,
        cb: CallbackId,
        phys: &mut Option<(i32, i32)>,
    ) -> bool {
        if let RawEvent::Wire {
            object_id, opcode, ..
        } = &event
            && *object_id == cb.raw()
            && CallbackEvent::from_opcode(*opcode) == Some(CallbackEvent::Done)
        {
            return true;
        }

        if self.record_output_property(&event, output, phys) {
            return false;
        }

        // Taken by value so fd-bearing events survive intact: keymap fds are
        // correlated with the message before them by arrival order, and the
        // spill preserves that order.
        self.spill.push(event);
        false
    }

    /// Record `event` if it is a `wl_output` property of `output`.
    ///
    /// Returns `true` when the event was consumed as such a property.
    fn record_output_property(
        &mut self,
        event: &RawEvent,
        output: OutputId,
        phys: &mut Option<(i32, i32)>,
    ) -> bool {
        let bytes = match event {
            RawEvent::Bytes(b) => b.as_ref(),
            RawEvent::Wire { bytes, .. } => bytes.as_ref(),
            RawEvent::Repeat | RawEvent::Fd(_) => return false,
        };
        let Some((mut ev, _)) = WireEvent::parse(bytes) else {
            return false;
        };
        if ev.object_id != output.raw() {
            return false;
        }
        match OutputEvent::from_opcode(ev.opcode) {
            Some(OutputEvent::Mode) => {
                let flags = ev.u32();
                let w = ev.i32();
                let h = ev.i32();
                if flags & Self::MODE_CURRENT != 0 && w > 0 && h > 0 {
                    *phys = Some((w, h));
                }
                true
            }
            Some(OutputEvent::Scale) => {
                self.scale = ev.i32().max(1);
                true
            }
            _ => false,
        }
    }

    /// The output's logical size, or `None` if it reported no usable mode.
    #[must_use]
    pub const fn logical_size(&self) -> Option<(i32, i32)> {
        self.logical
    }

    /// The output's scale factor, defaulting to `1`.
    #[must_use]
    pub const fn scale(&self) -> i32 {
        self.scale
    }

    /// Take the events read past during the probe, in arrival order, for the
    /// caller to replay through its own dispatch.
    #[must_use]
    pub fn take_spill(&mut self) -> Vec<RawEvent> {
        std::mem::take(&mut self.spill)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl OutputProbe {
        fn probe_for_test() -> Self {
            Self {
                spill: Vec::new(),
                logical: None,
                scale: 1,
            }
        }
    }

    fn mode_event(object_id: u32, w: i32, h: i32) -> RawEvent {
        let mut body = Vec::new();
        body.extend_from_slice(&OutputProbe::MODE_CURRENT.to_ne_bytes());
        body.extend_from_slice(&w.to_ne_bytes());
        body.extend_from_slice(&h.to_ne_bytes());
        body.extend_from_slice(&60_000i32.to_ne_bytes());
        wire_message(object_id, OutputEvent::Mode as u16, &body)
    }

    fn scale_event(object_id: u32, scale: i32) -> RawEvent {
        wire_message(object_id, OutputEvent::Scale as u16, &scale.to_ne_bytes())
    }

    fn wire_message(object_id: u32, opcode: u16, body: &[u8]) -> RawEvent {
        let len = u16::try_from(8 + body.len()).expect("test message fits");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&object_id.to_ne_bytes());
        bytes.extend_from_slice(&opcode.to_ne_bytes());
        bytes.extend_from_slice(&len.to_ne_bytes());
        bytes.extend_from_slice(body);
        RawEvent::Bytes(bytes.into_boxed_slice())
    }

    #[test]
    fn output_mode_and_scale_divide_into_a_logical_size() {
        let mut probe = OutputProbe::probe_for_test();
        let mut phys = None;
        let output = OutputId(7);
        probe.absorb(scale_event(7, 2), output, CallbackId(99), &mut phys);
        probe.absorb(mode_event(7, 3840, 2160), output, CallbackId(99), &mut phys);
        probe.logical = phys.map(|(w, h)| (w / probe.scale, h / probe.scale));

        assert_eq!(probe.logical_size(), Some((1920, 1080)));
        assert_eq!(probe.scale(), 2);
    }

    #[test]
    fn a_seat_event_arriving_mid_probe_is_kept_for_the_caller() {
        let mut probe = OutputProbe::probe_for_test();
        let mut phys = None;
        let seat_capabilities = RawEvent::Wire {
            object_id: 12,
            opcode: 0,
            args: [3, 0, 0, 0],
            nargs: 1,
            bytes: Box::new([]),
        };

        probe.absorb(seat_capabilities, OutputId(7), CallbackId(99), &mut phys);

        let spill = probe.take_spill();
        assert_eq!(spill.len(), 1, "the seat event must survive the probe");
        assert!(matches!(
            spill[0],
            RawEvent::Wire {
                object_id: 12,
                nargs: 1,
                ..
            }
        ));
    }

    #[test]
    fn output_properties_are_consumed_rather_than_spilled() {
        let mut probe = OutputProbe::probe_for_test();
        let mut phys = None;
        probe.absorb(
            mode_event(7, 1920, 1080),
            OutputId(7),
            CallbackId(99),
            &mut phys,
        );
        probe.absorb(scale_event(7, 1), OutputId(7), CallbackId(99), &mut phys);

        assert!(probe.take_spill().is_empty());
    }

    #[test]
    fn the_sync_callback_ends_the_probe() {
        let mut probe = OutputProbe::probe_for_test();
        let mut phys = None;
        let done = RawEvent::Wire {
            object_id: 99,
            opcode: CallbackEvent::Done as u16,
            args: [0; 4],
            nargs: 1,
            bytes: Box::new([]),
        };

        assert!(probe.absorb(done, OutputId(7), CallbackId(99), &mut phys));
        assert!(probe.take_spill().is_empty());
    }

    #[test]
    fn another_outputs_properties_are_not_mistaken_for_ours() {
        let mut probe = OutputProbe::probe_for_test();
        let mut phys = None;
        probe.absorb(
            mode_event(8, 1280, 720),
            OutputId(7),
            CallbackId(99),
            &mut phys,
        );

        assert_eq!(phys, None);
        assert_eq!(probe.take_spill().len(), 1);
    }
}
