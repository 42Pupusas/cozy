//! One output-sized background surface that repaints every frame.
//!
//! The static wallpaper paints once per configure. This surface instead
//! re-arms `wl_surface.frame` inside every callback, so the compositor
//! paces playback at the output refresh rate. Two shm slots alternate, and
//! a slot the compositor still holds is never painted into.

use platform::Result;
use platform::conn::Sender;
use platform::id::{BufferId, CallbackId, CompositorId, LayerShellId, OutputId, PoolId, ShmId};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::protocol::{
    Anchor, BufferEvent, BufferRequest, CallbackEvent, Format, KeyboardInteractivity, Layer,
    LayerSurfaceEvent, ShmPoolRequest, SurfaceRequest,
};
use platform::surface::Frame;
use platform::sys::ShmBuffer;
use platform::wire::MessageBuilder;
use video_stress::{FitMode, FramePack, FramePainter, VideoLayout, VideoPalette};

const APP_ID: &str = "video-wallpaper";

/// The shm resources backing one configured size.
struct Buffers {
    frames: [Frame; 2],
    memory: ShmBuffer,
    pool: PoolId,
}

/// A background surface that plays a clip.
pub struct VideoSurface {
    output: OutputId,
    shm: ShmId,
    layer: LayerSurfaceHandle,
    buffers: Option<Buffers>,
    painter: Option<FramePainter>,
    dimensions: Option<(i32, i32)>,
    frame_callback: Option<CallbackId>,
    awaiting_callback: bool,
    fit: FitMode,
    palette: VideoPalette,
    last_slot: Option<usize>,
    painted: u64,
}

impl VideoSurface {
    /// Create an unconfigured full-output background surface.
    pub fn create(
        sender: &mut Sender,
        compositor: CompositorId,
        layer_shell: LayerShellId,
        shm: ShmId,
        output: OutputId,
        fit: FitMode,
        palette: VideoPalette,
    ) -> Result<Self> {
        let layer = LayerSurfaceHandle::create(
            sender,
            compositor,
            layer_shell,
            &LayerSurfaceConfig {
                layer: Layer::Background,
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
                exclusive_zone: Some(-1),
                margin: None,
                keyboard_interactivity: KeyboardInteractivity::None,
            },
        )?;
        Ok(Self {
            output,
            shm,
            layer,
            buffers: None,
            painter: None,
            dimensions: None,
            frame_callback: None,
            awaiting_callback: false,
            fit,
            palette,
            last_slot: None,
            painted: 0,
        })
    }

    /// The output this surface follows.
    #[must_use]
    pub const fn output(&self) -> OutputId {
        self.output
    }

    /// How many frames this surface has painted.
    #[must_use]
    pub const fn painted(&self) -> u64 {
        self.painted
    }

    /// The configured surface size, once the compositor has supplied one.
    #[must_use]
    pub const fn dimensions(&self) -> Option<(i32, i32)> {
        self.dimensions
    }

    /// Whether the surface may paint a new frame now.
    ///
    /// A surface waits for the compositor to answer the previous frame
    /// callback, so playback follows the output refresh rate instead of
    /// queueing frames the compositor has not shown.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.dimensions.is_some() && !self.awaiting_callback
    }

    /// Ask for another frame callback without painting.
    ///
    /// The callback chain drives the whole loop, so a surface that skips a
    /// paint must still re-arm. Dropping the callback would leave the loop
    /// with no further events and stall playback permanently.
    pub fn rearm(&mut self, sender: &mut Sender) -> Result<()> {
        if self.awaiting_callback || self.dimensions.is_none() {
            return Ok(());
        }
        let callback: CallbackId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(self.layer.surface, SurfaceRequest::Frame).new_id(callback),
        )?;
        sender.request(MessageBuilder::new(
            self.layer.surface,
            SurfaceRequest::Commit,
        ))?;
        self.frame_callback = Some(callback);
        self.awaiting_callback = true;
        Ok(())
    }

    /// Handle one event, and report whether it was this surface's frame
    /// callback.
    pub fn handle_event(
        &mut self,
        sender: &mut Sender,
        event: &platform::event::RawEvent,
    ) -> Result<bool> {
        if let Some(callback) = self.frame_callback
            && event.is(callback)
            && event.event::<CallbackEvent>() == Some(CallbackEvent::Done)
        {
            self.frame_callback = None;
            self.awaiting_callback = false;
            return Ok(true);
        }
        if let Some(buffers) = &mut self.buffers
            && let Some(frame) = buffers
                .frames
                .iter_mut()
                .find(|frame| event.is(frame.buffer))
        {
            if event.event::<BufferEvent>() == Some(BufferEvent::Release) {
                frame.free = true;
            }
            return Ok(false);
        }
        if event.is(self.layer.layer_surface)
            && event.event::<LayerSurfaceEvent>() == Some(LayerSurfaceEvent::Configure)
        {
            self.layer.ack_configure(sender, event.arg(0))?;
            self.reconfigure(
                sender,
                event.arg(1).cast_signed(),
                event.arg(2).cast_signed(),
            )?;
        }
        Ok(false)
    }

    /// Rebuild the shm chain for a new size.
    fn reconfigure(&mut self, sender: &mut Sender, width: i32, height: i32) -> Result<()> {
        if width <= 0 || height <= 0 || self.dimensions == Some((width, height)) {
            return Ok(());
        }
        let (frames, memory, pool) =
            Frame::create_pair(sender, self.shm, width, height, Format::Xrgb8888)?;
        if let Some(old) = self.buffers.replace(Buffers {
            frames,
            memory,
            pool,
        }) {
            Self::destroy_buffers(sender, &old)?;
        }
        self.dimensions = Some((width, height));
        self.last_slot = None;
        self.frame_callback = None;
        self.awaiting_callback = false;
        Ok(())
    }

    /// Build the painter for the configured size and the clip geometry.
    ///
    /// The painter depends only on the surface size, so it is rebuilt after
    /// a configure and reused for every frame until the next one.
    pub fn fit_to(&mut self, pack: &FramePack) {
        let Some((width, height)) = self.dimensions else {
            return;
        };
        if self
            .painter
            .as_ref()
            .is_some_and(|painter| painter.covers(width.cast_unsigned(), height.cast_unsigned()))
        {
            return;
        }
        let header = pack.header();
        let layout = VideoLayout::place(
            width.cast_unsigned(),
            height.cast_unsigned(),
            header.width,
            header.height,
            self.fit,
        );
        self.painter = Some(FramePainter::new(
            layout,
            header.width,
            header.height,
            self.palette.unlit(),
            self.palette.lit(),
            self.palette.pad(),
        ));
    }

    /// Paint frame `index` of `pack` and commit it.
    ///
    /// Returns `false` when the compositor holds both slots, which leaves
    /// the frame unpainted rather than overwriting a buffer still on screen.
    pub fn present(&mut self, sender: &mut Sender, pack: &FramePack, index: usize) -> Result<bool> {
        let (Some(painter), Some((width, height))) = (&self.painter, self.dimensions) else {
            return Ok(false);
        };
        let Some(buffers) = &mut self.buffers else {
            return Ok(false);
        };
        let Some(slot) = Self::free_slot(&buffers.frames, self.last_slot) else {
            return Ok(false);
        };
        let Some(frame) = pack.frame(index) else {
            return Ok(false);
        };
        painter.paint(&frame, buffers.memory.pixels(slot));
        buffers.frames[slot].free = false;
        let buffer = buffers.frames[slot].buffer;
        self.last_slot = Some(slot);
        self.painted += 1;
        self.commit(sender, buffer, width, height)
    }

    /// Choose a free slot, preferring the one not currently on screen.
    fn free_slot(frames: &[Frame; 2], last: Option<usize>) -> Option<usize> {
        last.map(|slot| 1 - slot)
            .filter(|slot| frames[*slot].free)
            .or_else(|| frames.iter().position(|frame| frame.free))
    }

    /// Attach, damage, arm the next frame callback, and commit.
    fn commit(
        &mut self,
        sender: &mut Sender,
        buffer: BufferId,
        width: i32,
        height: i32,
    ) -> Result<bool> {
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
        let callback: CallbackId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(self.layer.surface, SurfaceRequest::Frame).new_id(callback),
        )?;
        sender.request(MessageBuilder::new(
            self.layer.surface,
            SurfaceRequest::Commit,
        ))?;
        self.frame_callback = Some(callback);
        self.awaiting_callback = true;
        Ok(true)
    }

    /// Release every protocol object this surface owns.
    pub fn destroy(&mut self, sender: &mut Sender) -> Result<()> {
        if let Some(buffers) = self.buffers.take() {
            Self::destroy_buffers(sender, &buffers)?;
        }
        self.layer.destroy(sender)?;
        Ok(())
    }

    /// Destroy one obsolete shm chain.
    fn destroy_buffers(sender: &mut Sender, buffers: &Buffers) -> Result<()> {
        for frame in &buffers.frames {
            sender.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy))?;
        }
        sender.request(MessageBuilder::new(buffers.pool, ShmPoolRequest::Destroy))?;
        Ok(())
    }
}
