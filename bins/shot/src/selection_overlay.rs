//! The interactive region picker: a fullscreen layer surface the user drags a
//! rectangle on.
//!
//! This is the `slurp` half of the `slurp | grim -g -` pipe, in-process. It
//! owns only the Wayland side -- surface, layer role, seat devices, shm
//! frames, event loop -- and delegates every decision: what the drag means to
//! [`DragSelection`], what it looks like to [`SelectionPainter`], and showing
//! the screen as it was before the overlay appeared to [`FrozenBackdrop`].
//! Keeping the logic out of here is what lets the interesting parts be tested
//! without a compositor, since this module fundamentally cannot be.

use crate::drag_selection::{DragSelection, SelectionOutcome};
use crate::frozen_backdrop::FrozenBackdrop;
use crate::keyboard_grab::KeyboardGrab;
use crate::overlay_deadlines::OverlayDeadlines;
use crate::overlay_globals::OverlayGlobals;
use crate::seat_devices::SeatDevices;
use crate::selection_painter::SelectionPainter;
use platform::Result;
use platform::conn::Sender;
use platform::draw::Canvas;
use platform::event::RawEvent;
use platform::id::{OutputId, PoolId, ShmId};
use platform::keyboard_decoder::{KeyboardDecoder, KeyboardUpdate};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::protocol::{
    Anchor, ButtonState, Format, Layer, LayerSurfaceEvent, PointerButton,
    PointerEvent, SurfaceRequest,
};
use platform::protocol::{BufferEvent, BufferRequest, ShmPoolRequest};
use platform::surface::Frame;
use platform::sys::ShmBuffer;
use platform::toplevel::Key;
use platform::wire::MessageBuilder;
use quetzalcoatl::spsc::Consumer;
use screencopy::{CaptureRegion, HeldFrame};
use std::time::Instant;

const APP_ID: &str = "shot";

/// A fullscreen overlay on one output, driven until the user picks or cancels.
///
/// Borrows the frozen capture for `'f`, so the shm pool the backdrop
/// displays outlives the overlay.
pub struct SelectionOverlay<'f> {
    layer: LayerSurfaceHandle,
    frozen: HeldFrame<'f>,
    backdrop: Option<FrozenBackdrop>,
    /// Devices bound *before* the surface was mapped, so Escape and clicks
    /// are guaranteed to have somewhere to arrive.
    devices: SeatDevices,
    shm: ShmId,
    /// Surface size in pixels, learned from the layer-shell `configure`.
    size: Option<(i32, i32)>,
    frames: Option<([Frame; 2], ShmBuffer, PoolId)>,
    selection: DragSelection,
    painter: SelectionPainter,
    keyboard: KeyboardDecoder,
    grab: KeyboardGrab,
    /// Set when the compositor closes the surface out from under us.
    closed: bool,
    deadlines: OverlayDeadlines,
}

impl<'f> SelectionOverlay<'f> {
    /// Create the overlay surface, anchored to all four edges of `output`,
    /// over a backdrop showing `frozen`.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn create(
        sender: &mut Sender,
        globals: &OverlayGlobals,
        devices: SeatDevices,
        output: OutputId,
        frozen: HeldFrame<'f>,
    ) -> Result<Self> {
        let grab = KeyboardGrab::new(devices.can_cancel_by_key());
        let layer = LayerSurfaceHandle::create(
            sender,
            globals.compositor,
            globals.layer_shell,
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
                keyboard_interactivity: KeyboardGrab::initial(),
            },
        )?;
        let backdrop = FrozenBackdrop::create(sender, globals, layer.surface)?;
        Ok(Self {
            layer,
            frozen,
            backdrop,
            devices,
            shm: globals.shm,
            size: None,
            frames: None,
            selection: DragSelection::new(0, 0),
            painter: SelectionPainter::default(),
            keyboard: KeyboardDecoder::new(),
            grab,
            closed: false,
            deadlines: OverlayDeadlines::starting_at(Instant::now()),
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
            let configured = self.size.is_some();
            if let Some(expiry) = self.deadlines.expired(Instant::now(), configured) {
                self.destroy(sender)?;
                eprintln!("shot: {}", expiry.message());
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
            let _ = wake.wait(self.deadlines.wait_ms(Instant::now(), configured));
            wake.drain();
        }
    }

    /// The overlay's logical size, the space a picked region is given in.
    #[must_use]
    pub const fn surface_size(&self) -> Option<(i32, i32)> {
        self.size
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
        if let Some(backdrop) = self.backdrop.take() {
            backdrop.destroy(sender)?;
        }
        self.layer.destroy(sender)?;
        Ok(())
    }

    /// Route one event to whichever object it belongs to.
    fn handle_event(&mut self, sender: &mut Sender, ev: &RawEvent) -> Result<()> {
        if let RawEvent::Fd(fds) = ev {
            if self.keyboard.absorb_keymap_fd(fds) {
                self.grant_keyboard(sender)?;
            }
            return Ok(());
        }
        if ev.is(self.layer.layer_surface) {
            return self.handle_layer_surface(sender, ev);
        }
        if self.devices.pointer().is_some_and(|p| ev.is(p)) {
            self.deadlines.input_at(Instant::now());
            self.handle_pointer(ev);
            return self.repaint(sender);
        }
        if self.devices.keyboard().is_some_and(|k| ev.is(k)) {
            self.deadlines.input_at(Instant::now());
            self.handle_keyboard(ev);
            return Ok(());
        }
        self.release_buffer(ev);
        Ok(())
    }

    /// Take exclusive keyboard focus once Escape can actually be decoded.
    fn grant_keyboard(&mut self, sender: &mut Sender) -> Result<()> {
        let Some(mode) = self.grab.on_keymap(self.keyboard.has_keymap()) else {
            return Ok(());
        };
        self.layer.set_keyboard_interactivity(sender, mode)?;
        sender.request(MessageBuilder::new(
            self.layer.surface,
            SurfaceRequest::Commit,
        ))?;
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
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.show(sender, &self.frozen, (width, height))?;
        }
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

    fn handle_keyboard(&mut self, ev: &RawEvent) {
        if let KeyboardUpdate::Key(input) = self.keyboard.observe(ev)
            && input.is_press()
            && input.key == Some(Key::Escape)
        {
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
