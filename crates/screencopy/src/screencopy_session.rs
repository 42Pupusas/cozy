//! The live half of a capture: object ids, an shm pool, and the wire loop.
//!
//! Everything that can be decided without a compositor already lives
//! elsewhere -- [`FrameHandshake`] decides *when* to copy, [`FrameParams`]
//! decides whether an offer is usable, [`CapturedFrame`] converts the bytes.
//! What's left here is genuinely I/O-shaped and is kept deliberately thin:
//! allocate ids, create the buffer the handshake asked for, route wire events
//! to the state machine, and hand back pixels.
//!
//! Note there is deliberately *no* `wl_display.sync` round-trip during a
//! capture. `capture_output` grabs the **next** frame, so the compositor sends
//! its `buffer` events whenever it next composites -- which is routinely after
//! a sync `done` would have fired. An earlier version of this code used a
//! round-trip to close the parameter menu and consequently failed every
//! capture with "no shared-memory buffer offered"; the menu's end is decided
//! by interface version instead. See `frame_handshake`.

use platform::conn::Sender;
use platform::error::Error as PlatformError;
use platform::event::RawEvent;
use platform::id::{
    BufferId, DisplayId, ObjectId, OutputId, PoolId, ScreencopyFrameId, ScreencopyManagerId,
    ShmId,
};
use platform::protocol::{
    DisplayEvent, ScreencopyFrameEvent, ScreencopyFrameRequest, ScreencopyManagerRequest,
    ShmPoolRequest, ShmRequest,
};
use platform::sys::ShmBuffer;
use platform::wire::{MessageBuilder, WireEvent};
use quetzalcoatl::spsc::Consumer;

use crate::capture_error::{Result, ScreencopyError};
use crate::captured_frame::CapturedFrame;
use crate::frame_handshake::{FrameHandshake, Step};
use crate::frame_params::FrameParams;

/// The rectangle to capture, in an output's logical coordinates.
///
/// Kept distinct from a bare tuple because `capture_output_region` takes the
/// four numbers positionally, and transposing x/y or width/height there yields
/// a plausible-looking screenshot of the wrong place.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CaptureRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Drives `zwlr_screencopy_manager_v1` to pull pixels off one output.
///
/// Holds the bound manager and the pool reused between captures, so a caller
/// grabbing several outputs in one run pays for the pool once.
pub struct ScreencopySession {
    manager: ScreencopyManagerId,
    /// The version `manager` was bound at. Below 3 there is no `buffer_done`,
    /// which changes how the parameter menu ends.
    version: u32,
    shm: ShmId,
    /// The shm pool backing captures, grown on demand. `None` until the first
    /// capture, because the compositor -- not us -- decides how big it is.
    pool: Option<CapturePool>,
}

/// The shm pool and the `wl_buffer` currently carved out of it.
struct CapturePool {
    memory: ShmBuffer,
    pool: PoolId,
    buffer: BufferId,
    /// The parameters `buffer` was created with, so a capture whose geometry
    /// matches can reuse it instead of re-creating the object.
    params: FrameParams,
}

impl ScreencopySession {
    /// Wrap an already-bound `zwlr_screencopy_manager_v1` and `wl_shm`.
    ///
    /// `version` must be the version the manager was actually bound at (the
    /// minimum of what we support and what the compositor advertised), since
    /// it decides whether `buffer_done` will ever arrive.
    #[must_use]
    pub const fn new(manager: ScreencopyManagerId, version: u32, shm: ShmId) -> Self {
        Self {
            manager,
            version,
            shm,
            pool: None,
        }
    }

    /// Capture `output`, optionally restricted to `region`, blocking until
    /// the pixels arrive.
    ///
    /// `overlay_cursor` composites the pointer into the image, matching
    /// `grim -c`.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures, and reports a compositor `failed`
    /// event or an uninterpretable buffer offer as [`Error::Screencopy`].
    pub fn capture(
        &mut self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        output: OutputId,
        region: Option<CaptureRegion>,
        overlay_cursor: bool,
    ) -> Result<CapturedFrame> {
        let frame = self.request_frame(sender, output, region, overlay_cursor)?;
        let mut handshake = FrameHandshake::new(self.version);
        let result = self.run_handshake(sender, events, frame, &mut handshake);
        // A frame object is single-use and must be destroyed whether it
        // succeeded or failed, or the compositor leaks it for our lifetime.
        let _ = sender.request(MessageBuilder::new(frame, ScreencopyFrameRequest::Destroy));
        result
    }

    /// Ask the compositor for a frame covering `output` (or part of it).
    fn request_frame(
        &self,
        sender: &mut Sender,
        output: OutputId,
        region: Option<CaptureRegion>,
        overlay_cursor: bool,
    ) -> Result<ScreencopyFrameId> {
        let frame: ScreencopyFrameId = sender.alloc_id();
        let cursor = i32::from(overlay_cursor);
        match region {
            Some(r) => sender.request(
                MessageBuilder::new(self.manager, ScreencopyManagerRequest::CaptureOutputRegion)
                    .new_id(frame)
                    .i32(cursor)
                    .object(output)
                    .i32(r.x)
                    .i32(r.y)
                    .i32(r.width)
                    .i32(r.height),
            )?,
            None => sender.request(
                MessageBuilder::new(self.manager, ScreencopyManagerRequest::CaptureOutput)
                    .new_id(frame)
                    .i32(cursor)
                    .object(output),
            )?,
        }
        Ok(frame)
    }

    /// Pump events until the handshake settles, acting on each [`Step`].
    fn run_handshake(
        &mut self,
        sender: &mut Sender,
        events: &mut Consumer<RawEvent>,
        frame: ScreencopyFrameId,
        handshake: &mut FrameHandshake,
    ) -> Result<CapturedFrame> {
        loop {
            let event = events.pop_block().ok_or(PlatformError::ReaderStopped)?;
            // `wl_display.error` is the one message that must never be
            // ignored: the compositor has already dropped the object, so
            // waiting for a reply that will never come would hang.
            if let RawEvent::Bytes(bytes) = &event {
                if let Some((mut ev, _)) = WireEvent::parse(bytes)
                    && ev.object_id == DisplayId(1).raw()
                    && DisplayEvent::from_opcode(ev.opcode) == Some(DisplayEvent::Error)
                {
                    return Err(PlatformError::Protocol {
                        object: ev.u32(),
                        code: ev.u32(),
                        message: ev.string(),
                    }
                    .into());
                }
                continue;
            }
            // Everything else about our frame arrives as `Wire`: the reader
            // only falls back to `Bytes` for `wl_display`/`wl_registry`, so
            // every screencopy event -- including the four-argument `buffer`
            // -- has its arguments inlined here.
            if !event.is(frame) {
                continue;
            }
            let step = Self::route_frame_event(&event, handshake);

            match step {
                Step::Wait => {}
                Step::SendCopy(params) => {
                    let buffer = self.prepare_buffer(sender, params)?;
                    sender.request(
                        MessageBuilder::new(frame, ScreencopyFrameRequest::Copy).object(buffer),
                    )?;
                }
                Step::Done { flags } => {
                    let params = handshake.committed().ok_or(ScreencopyError::NoBuffer)?;
                    return self.read_captured(params, flags);
                }
                Step::Failed(e) => return Err(e.into()),
            }
        }
    }

    /// Translate one `zwlr_screencopy_frame_v1` event into a [`Step`].
    fn route_frame_event(ev: &RawEvent, handshake: &mut FrameHandshake) -> Step {
        match ev.event::<ScreencopyFrameEvent>() {
            Some(ScreencopyFrameEvent::Buffer) => {
                handshake.on_buffer(ev.arg(0), ev.arg(1), ev.arg(2), ev.arg(3))
            }
            Some(ScreencopyFrameEvent::BufferDone) => handshake.on_buffer_done(),
            Some(ScreencopyFrameEvent::Flags) => handshake.on_flags(ev.arg(0)),
            Some(ScreencopyFrameEvent::Ready) => handshake.on_ready(),
            Some(ScreencopyFrameEvent::Failed) => handshake.on_failed(),
            // `damage` only follows `copy_with_damage`, which we never send;
            // `linux_dmabuf` is a buffer type this shm client cannot use.
            Some(ScreencopyFrameEvent::Damage | ScreencopyFrameEvent::LinuxDmabuf) | None => {
                Step::Wait
            }
        }
    }

    /// Ensure a `wl_buffer` matching `params` exists, creating or growing the
    /// pool as needed, and return it.
    fn prepare_buffer(&mut self, sender: &mut Sender, params: FrameParams) -> Result<BufferId> {
        if let Some(existing) = &self.pool {
            // Repeat captures of the same output hit this: same geometry, so
            // the buffer object is still exactly right.
            if existing.params == params {
                return Ok(existing.buffer);
            }
        }
        // Geometry changed (or this is the first capture): drop the old
        // objects and rebuild. Screenshots are one-shot enough that reusing a
        // mismatched pool isn't worth the bookkeeping.
        if let Some(old) = self.pool.take() {
            let _ = sender.request(MessageBuilder::new(old.pool, ShmPoolRequest::Destroy));
        }
        self.pool = Some(CapturePool::create(sender, self.shm, params)?);
        Ok(self.pool.as_ref().expect("just created").buffer)
    }

    /// Convert the pool's bytes into an encoder-ready image.
    fn read_captured(&mut self, params: FrameParams, flags: u32) -> Result<CapturedFrame> {
        // `Done` is only reachable after `SendCopy` built the pool.
        let pool = self.pool.as_mut().ok_or(ScreencopyError::NoBuffer)?;
        Ok(CapturedFrame::from_shm(
            &params,
            flags,
            pool.memory.bytes(0),
        )?)
    }
}

impl CapturePool {
    /// Allocate a pool sized to `params` and carve one `wl_buffer` from it.
    fn create(sender: &mut Sender, shm: ShmId, params: FrameParams) -> Result<Self> {
        // `ShmBuffer` sizes itself in whole `width * height` pixels, but the
        // compositor's stride may include row padding. Requesting the padded
        // stride as the width buys exactly the bytes it will write; anything
        // smaller and the final row runs off the end of the mapping.
        let padded_width = params.stride as usize / 4;
        let memory = ShmBuffer::new(padded_width, params.height as usize, 1)
            .map_err(|e| PlatformError::Conn(platform::conn::ConnError::Sys(e)))?;

        let pool: PoolId = sender.alloc_id();
        let msg = MessageBuilder::new(shm, ShmRequest::CreatePool)
            .new_id(pool)
            .i32(i32::try_from(memory.size_bytes()).unwrap_or(i32::MAX))
            .finish();
        sender.send_with_fds(&msg, &[memory.fd()])?;

        let buffer: BufferId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(pool, ShmPoolRequest::CreateBuffer)
                .new_id(buffer)
                .i32(0)
                .i32(i32::try_from(params.width).unwrap_or(i32::MAX))
                .i32(i32::try_from(params.height).unwrap_or(i32::MAX))
                .i32(i32::try_from(params.stride).unwrap_or(i32::MAX))
                .u32(params.format.code()),
        )?;

        Ok(Self {
            memory,
            pool,
            buffer,
            params,
        })
    }
}
