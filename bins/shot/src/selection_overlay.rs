//! The interactive region picker: a fullscreen layer surface the user drags a
//! rectangle on.
//!
//! This is the `slurp` half of the `slurp | grim -g -` pipe, in-process. It
//! owns only the Wayland side -- surface, layer role, seat devices, shm
//! frames, event loop -- and delegates every decision: what the drag means to
//! [`DragSelection`], what it looks like to [`SelectionPainter`]. Keeping the
//! logic out of here is what lets the interesting parts be tested without a
//! compositor, since this module fundamentally cannot be.

use crate::drag_selection::{DragSelection, SelectionOutcome};
use crate::seat_devices::SeatDevices;
use crate::selection_painter::SelectionPainter;
use platform::Result;
use platform::conn::Sender;
use platform::draw::Canvas;
use platform::event::RawEvent;
use platform::id::{CompositorId, LayerShellId, OutputId, PoolId, ShmId};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::protocol::{
    Anchor, ButtonState, Format, KeyState, KeyboardEvent, KeyboardInteractivity, Layer,
    LayerSurfaceEvent, PointerButton, PointerEvent, SurfaceRequest,
};
use platform::protocol::{BufferEvent, BufferRequest, ShmPoolRequest};
use platform::surface::Frame;
use platform::sys::ShmBuffer;
use platform::wire::MessageBuilder;
use quetzalcoatl::spsc::Consumer;
use screencopy::CaptureRegion;
use std::time::{Duration, Instant};

/// The evdev code for Escape, which cancels the selection.
///
/// Matched as a raw code rather than through an XKB translator on purpose:
/// Escape sits at the same physical position in every layout, and a picker
/// that needed a keymap round-trip before it could be dismissed would be a
/// trap if that parse ever failed.
const KEY_ESCAPE: u32 = 1;

const APP_ID: &str = "shot";

/// How long the picker may stay on screen without the compositor ever
/// configuring it.
///
/// This is a deadman switch, not a UX timeout. A fullscreen `Overlay`-layer
/// surface holding exclusive keyboard focus is indistinguishable from a
/// locked screen if input never arrives, and the user's only recourse is a
/// hard restart -- which is exactly what happened once. If the surface has
/// not even been configured within this window, something is wrong enough
/// that vanishing is strictly better than persisting.
const CONFIGURE_DEADLINE: Duration = Duration::from_secs(5);

/// How long the picker may stay mapped with no input event at all.
///
/// Generous, because a user genuinely may take their time aiming. It only
/// fires when *nothing* arrives -- any pointer motion or key press resets it
/// -- so a live picker never hits it, while a picker whose input is going
/// somewhere else cannot outlive it.
const IDLE_DEADLINE: Duration = Duration::from_mins(2);

/// A fullscreen overlay on one output, driven until the user picks or cancels.
pub struct SelectionOverlay {
    layer: LayerSurfaceHandle,
    /// Devices bound *before* the surface was mapped, so Escape and clicks
    /// are guaranteed to have somewhere to arrive.
    devices: SeatDevices,
    shm: ShmId,
    /// Surface size in pixels, learned from the layer-shell `configure`.
    size: Option<(i32, i32)>,
    frames: Option<([Frame; 2], ShmBuffer, PoolId)>,
    selection: DragSelection,
    painter: SelectionPainter,
    /// Set when the compositor closes the surface out from under us.
    closed: bool,
    /// When the overlay was created, for the configure deadman switch.
    created: Instant,
    /// Last time any input event arrived, for the idle deadman switch.
    last_input: Instant,
}

impl SelectionOverlay {
    /// Create the overlay surface, anchored to all four edges of `output`.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn create(
        sender: &mut Sender,
        compositor: CompositorId,
        layer_shell: LayerShellId,
        devices: SeatDevices,
        shm: ShmId,
        output: OutputId,
    ) -> Result<Self> {
        // Exclusive keyboard focus so Escape reaches us rather than whatever
        // was focused underneath -- but *only* when a keyboard was actually
        // bound. Taking exclusive focus without one is what makes an
        // undismissable screen: the compositor dutifully routes every key to
        // a client that has no keyboard object to receive them, so Escape
        // reaches nothing and no other window can be focused either.
        let interactivity = if devices.can_cancel_by_key() {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        };
        let layer = LayerSurfaceHandle::create(
            sender,
            compositor,
            layer_shell,
            &LayerSurfaceConfig {
                // Overlay, not Top: the picker must float above every other
                // layer-shell client (bars, notifications) or the user
                // cannot select the pixels those cover.
                layer: Layer::Overlay,
                namespace: APP_ID,
                output,
                buffer_scale: None,
                anchor: Some(Anchor::bits(&[
                    Anchor::Top,
                    Anchor::Bottom,
                    Anchor::Left,
                    Anchor::Right,
                ])),
                size: None,
                // `-1` means "cover the whole output, ignoring other clients'
                // exclusive zones" -- without it a status bar would inset the
                // picker and the strip behind the bar could never be selected.
                exclusive_zone: Some(-1),
                margin: None,
                keyboard_interactivity: interactivity,
            },
        )?;
        let now = Instant::now();
        Ok(Self {
            layer,
            devices,
            shm,
            size: None,
            frames: None,
            selection: DragSelection::new(0, 0),
            painter: SelectionPainter::default(),
            closed: false,
            created: now,
            last_input: now,
        })
    }

    /// Run the picker until the user commits a region or cancels it.
    ///
    /// Returns `None` when cancelled (Escape, right-click, a bare click, or
    /// the compositor closing the surface), so the caller can exit quietly
    /// rather than treating a deliberate dismissal as an error.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn pick(
        &mut self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        wake: &waker::Waker,
    ) -> Result<Option<CaptureRegion>> {
        loop {
            while let Some(event) = events.pop() {
                self.handle_event(sender, &event)?;
            }
            if self.closed {
                return Ok(None);
            }
            if let Some(reason) = self.expired() {
                // Tear down first, then report: leaving a surface up while
                // printing a diagnostic is the failure we are guarding
                // against.
                self.destroy(sender)?;
                eprintln!("shot: {reason}");
                return Ok(None);
            }
            match self.selection.outcome() {
                SelectionOutcome::Committed(region) => {
                    // Take the surface down *before* returning: the capture
                    // that follows must not photograph our own dimming.
                    self.destroy(sender)?;
                    return Ok(Some(region));
                }
                SelectionOutcome::Cancelled => {
                    self.destroy(sender)?;
                    return Ok(None);
                }
                SelectionOutcome::Pending => {}
            }
            let _ = wake.wait(self.deadman_wait_ms());
            wake.drain();
        }
    }

    /// Milliseconds until the nearest deadman switch can trip, so the loop
    /// sleeps exactly that long when no event arrives.
    fn deadman_wait_ms(&self) -> i32 {
        let mut remaining = IDLE_DEADLINE.saturating_sub(self.last_input.elapsed());
        if self.size.is_none() {
            remaining = remaining.min(CONFIGURE_DEADLINE.saturating_sub(self.created.elapsed()));
        }
        i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1)
    }

    /// Whether a deadman switch has tripped, and why.
    ///
    /// Split out so the two deadlines are stated in one place rather than
    /// buried in the loop: this is the code that guarantees the picker cannot
    /// become a permanent fixture on the user's screen.
    fn expired(&self) -> Option<String> {
        if self.size.is_none() && self.created.elapsed() >= CONFIGURE_DEADLINE {
            return Some(
                "the compositor never configured the selection overlay; giving up rather \
                 than leaving an invisible surface covering the screen"
                    .to_string(),
            );
        }
        if self.last_input.elapsed() >= IDLE_DEADLINE {
            return Some("selection timed out with no input; cancelled".to_string());
        }
        None
    }

    /// Tear the overlay down and make sure the compositor has actually
    /// unmapped it before the caller captures.
    fn destroy(&mut self, sender: &mut Sender) -> Result<()> {
        if let Some((frames, _memory, pool)) = self.frames.take() {
            for frame in frames {
                sender.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy))?;
            }
            sender.request(MessageBuilder::new(pool, ShmPoolRequest::Destroy))?;
        }
        self.layer.destroy(sender)?;
        Ok(())
    }

    /// Route one event to whichever object it belongs to.
    fn handle_event(&mut self, sender: &mut Sender, ev: &RawEvent) -> Result<()> {
        if ev.is(self.layer.layer_surface) {
            return self.handle_layer_surface(sender, ev);
        }
        if self.devices.pointer().is_some_and(|p| ev.is(p)) {
            self.last_input = Instant::now();
            self.handle_pointer(ev);
            return self.repaint(sender);
        }
        if self.devices.keyboard().is_some_and(|k| ev.is(k)) {
            self.last_input = Instant::now();
            self.handle_keyboard(ev);
            return Ok(());
        }
        self.release_buffer(ev);
        Ok(())
    }

    /// Mark a frame free again when the compositor hands it back.
    ///
    /// Without this the second paint takes the last free frame and every
    /// later one falls back to index 0 -- drawing into a buffer the
    /// compositor is still scanning out, which tears the rubber band.
    fn release_buffer(&mut self, ev: &RawEvent) {
        let Some((frames, _memory, _pool)) = self.frames.as_mut() else {
            return;
        };
        for frame in frames.iter_mut() {
            if ev.is(frame.buffer) && ev.event::<BufferEvent>() == Some(BufferEvent::Release) {
                frame.free = true;
            }
        }
    }

    /// `configure` carries the size the compositor chose; `closed` means the
    /// surface is gone.
    fn handle_layer_surface(&mut self, sender: &mut Sender, ev: &RawEvent) -> Result<()> {
        match ev.event::<LayerSurfaceEvent>() {
            Some(LayerSurfaceEvent::Configure) => {
                let serial = ev.arg(0);
                self.layer.ack_configure(sender, serial)?;
                let width = ev.arg(1).cast_signed();
                let height = ev.arg(2).cast_signed();
                if width > 0 && height > 0 {
                    self.resize(sender, width, height)?;
                }
                Ok(())
            }
            Some(LayerSurfaceEvent::Closed) => {
                self.closed = true;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Allocate (or reallocate) the shm frames for a new surface size.
    fn resize(&mut self, sender: &mut Sender, width: i32, height: i32) -> Result<()> {
        if self.size == Some((width, height)) {
            return Ok(());
        }
        if let Some((frames, _memory, pool)) = self.frames.take() {
            for frame in frames {
                sender.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy))?;
            }
            sender.request(MessageBuilder::new(pool, ShmPoolRequest::Destroy))?;
        }
        // Argb8888, not Xrgb8888: the whole overlay depends on the compositor
        // honouring per-pixel alpha so the desktop shows through the dim and
        // the selection is genuinely clear.
        self.frames = Some(Frame::create_pair(
            sender,
            self.shm,
            width,
            height,
            Format::Argb8888,
        )?);
        self.size = Some((width, height));
        self.selection = DragSelection::new(width, height);
        self.repaint(sender)
    }

    /// Pointer events drive the drag state machine.
    fn handle_pointer(&mut self, ev: &RawEvent) {
        match ev.event::<PointerEvent>() {
            // Enter args: serial, surface, x, y.
            Some(PointerEvent::Enter) => self.selection.motion(ev.point(2, 3)),
            // Motion args: time, x, y.
            Some(PointerEvent::Motion) => self.selection.motion(ev.point(1, 2)),
            Some(PointerEvent::Button) => self.handle_button(ev),
            _ => {}
        }
    }

    /// Button args: serial, time, button, state.
    fn handle_button(&mut self, ev: &RawEvent) {
        let button = PointerButton::from_code(ev.arg(2));
        let state = ButtonState::from_raw(ev.arg(3));
        let at = self.selection.cursor();
        match (button, state) {
            (Some(PointerButton::Left), Some(ButtonState::Pressed)) => self.selection.press(at),
            (Some(PointerButton::Left), Some(ButtonState::Released)) => {
                self.selection.release(at);
            }
            // Right-click is the conventional "get me out of here" for a
            // picker, and costs nothing to honour.
            (Some(PointerButton::Right), Some(ButtonState::Pressed)) => self.selection.cancel(),
            _ => {}
        }
    }

    /// Escape cancels. Key args: serial, time, key (evdev code), state.
    fn handle_keyboard(&mut self, ev: &RawEvent) {
        if ev.event::<KeyboardEvent>() != Some(KeyboardEvent::Key) {
            return;
        }
        if ev.arg(2) == KEY_ESCAPE && KeyState::from_raw(ev.arg(3)) == Some(KeyState::Pressed) {
            self.selection.cancel();
        }
    }

    /// Paint the current selection into a free frame and present it.
    fn repaint(&mut self, sender: &mut Sender) -> Result<()> {
        let Some((width, height)) = self.size else {
            return Ok(());
        };
        // A committed/cancelled selection is about to tear the surface down;
        // painting it would only risk showing the overlay in the capture.
        if self.selection.is_finished() {
            return Ok(());
        }
        let Some((frames, memory, _pool)) = self.frames.as_mut() else {
            return Ok(());
        };
        let index = frames.iter().position(|f| f.free).unwrap_or(0);
        let pixels = memory.pixels(index);
        let mut canvas = Canvas::new(pixels, width, height);
        self.painter.paint(&mut canvas, &self.selection);
        let buffer = frames[index].buffer;
        frames[index].free = false;
        sender.request(
            MessageBuilder::new(self.layer.surface, SurfaceRequest::Attach)
                .object(buffer)
                .i32(0)
                .i32(0),
        )?;
        sender.request(
            MessageBuilder::new(self.layer.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(width)
                .i32(height),
        )?;
        sender.request(MessageBuilder::new(
            self.layer.surface,
            SurfaceRequest::Commit,
        ))?;
        Ok(())
    }
}
