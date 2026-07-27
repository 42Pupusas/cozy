//! One output-sized `Layer::Background` surface.
//!
//! This owner deliberately knows nothing about registry names or PNG decoding:
//! it receives one bound `wl_output`, waits for that output's layer-shell
//! configure, then owns the shm frame backing the rendered image. PNG decode
//! and scale-mode math stay in `wallpaper_pixels`; this module only owns the
//! layer-shell and shm lifetime contract.

use crate::appearance::Appearance;
use crate::wallpaper_pixels::WallpaperPixels;
use platform::Result;
use platform::conn::Sender;
use platform::id::{
    CompositorId, LayerShellId, LayerSurfaceId, OutputId, PoolId, ShmId, SurfaceId,
};
use platform::protocol::{
    Anchor, BufferRequest, CompositorRequest, Format, KeyboardInteractivity, Layer,
    LayerShellRequest, LayerSurfaceEvent, LayerSurfaceRequest, ShmPoolRequest, SurfaceRequest,
};
use platform::surface::Frame;
use platform::sys::ShmBuffer;
use platform::wire::MessageBuilder;

const APP_ID: &str = "wallpaper";

/// The objects and pixels for one compositor output's background.
pub struct BackgroundSurface {
    output: OutputId,
    shm: ShmId,
    surface: SurfaceId,
    layer_surface: LayerSurfaceId,
    frames: Option<([Frame; 2], ShmBuffer, PoolId)>,
    dimensions: Option<(i32, i32)>,
    appearance: Appearance,
    wallpaper: Option<WallpaperPixels>,
}

impl BackgroundSurface {
    /// Create an unconfigured surface stretched to all four output edges.
    pub fn create(
        sender: &mut Sender,
        compositor: CompositorId,
        layer_shell: LayerShellId,
        shm: ShmId,
        output: OutputId,
        appearance: Appearance,
    ) -> Result<Self> {
        let surface = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        let layer_surface = sender.alloc_id();
        sender.request(
            MessageBuilder::new(layer_shell, LayerShellRequest::GetLayerSurface)
                .new_id(layer_surface)
                .object(surface)
                .object(output)
                .u32(Layer::Background as u32)
                .string(APP_ID),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetAnchor).u32(Anchor::bits(
                &[Anchor::Top, Anchor::Bottom, Anchor::Left, Anchor::Right],
            )),
        )?;
        // `-1` is the layer-shell sentinel for a true full-output surface:
        // do not reserve usable space and do not be inset by another layer's
        // exclusive zone (such as a status bar). `0` merely reserves nothing;
        // it still permits the compositor to size this surface around a bar.
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetExclusiveZone).i32(-1),
        )?;
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetKeyboardInteractivity)
                .u32(KeyboardInteractivity::None as u32),
        )?;
        // An empty initial commit asks layer-shell for the output dimensions.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;
        let mut background = Self {
            output,
            shm,
            surface,
            layer_surface,
            frames: None,
            dimensions: None,
            appearance: appearance.clone(),
            wallpaper: None,
        };
        // Load the wildcard appearance before the first configure. A later
        // `wl_output.name` event can still replace it with a named rule.
        background.set_appearance(sender, appearance)?;
        Ok(background)
    }

    /// Destroy the role and its plain surface before forgetting an output.
    pub fn destroy(&mut self, sender: &mut Sender) -> Result<()> {
        if let Some(frames) = self.frames.take() {
            Self::destroy_frames(sender, frames)?;
        }
        sender.request(MessageBuilder::new(
            self.layer_surface,
            LayerSurfaceRequest::Destroy,
        ))?;
        sender.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy))?;
        Ok(())
    }

    /// The output this surface follows.
    #[must_use]
    pub const fn output(&self) -> OutputId {
        self.output
    }

    /// Apply the selected per-output appearance before its first configure.
    pub fn set_appearance(&mut self, sender: &mut Sender, appearance: Appearance) -> Result<()> {
        self.wallpaper = match WallpaperPixels::from_appearance(&appearance) {
            Ok(wallpaper) => wallpaper,
            Err(error) => {
                // A bad per-output file must not leave that output without a
                // background. Keep running with its explicitly configured
                // color while the diagnostic names the failed image.
                eprintln!("wallpaper: {error}; using configured solid color instead");
                None
            }
        };
        self.appearance = appearance;
        // Name events usually precede layer configure, but protocol ordering
        // does not make that a contract. Repaint immediately when this output
        // was already configured so named rules never remain at `*`.
        if let Some((width, height)) = self.dimensions {
            self.dimensions = None;
            self.reconfigure(sender, width, height)?;
        }
        Ok(())
    }

    /// Whether `event` targets this layer surface; if it is a configure, ack
    /// and present the selected background frame.
    pub fn handle_event(
        &mut self,
        sender: &mut Sender,
        event: &platform::event::RawEvent,
    ) -> Result<()> {
        if !event.is(self.layer_surface)
            || event.event::<LayerSurfaceEvent>() != Some(LayerSurfaceEvent::Configure)
        {
            return Ok(());
        }
        sender.request(
            MessageBuilder::new(self.layer_surface, LayerSurfaceRequest::AckConfigure)
                .u32(event.arg(0)),
        )?;
        // A zero dimension means the compositor left that axis client-chosen.
        // Four-edge anchoring normally supplies both; declining a zero-sized
        // shm allocation is safer than inventing geometry on a protocol edge.
        let width = event.arg(1).cast_signed();
        let height = event.arg(2).cast_signed();
        self.reconfigure(sender, width, height)
    }

    /// Replace the buffers and repaint after a size or appearance transition.
    fn reconfigure(&mut self, sender: &mut Sender, width: i32, height: i32) -> Result<()> {
        if width <= 0 || height <= 0 || self.dimensions == Some((width, height)) {
            return Ok(());
        }
        let new_frames = self.render_frames(sender, width, height)?;
        // A configure supersedes the old output geometry. Destroying client
        // buffer and pool objects is legal even if the compositor still uses
        // their contents: Wayland keeps the server-side resources alive until
        // its references are gone. This lets the mmap be released promptly.
        if let Some(old_frames) = self.frames.replace(new_frames) {
            Self::destroy_frames(sender, old_frames)?;
        }
        self.dimensions = Some((width, height));
        self.commit_first_frame(sender, width, height)
    }

    /// Allocate and fill a new two-buffer XRGB swap pair for one geometry.
    fn render_frames(
        &self,
        sender: &mut Sender,
        width: i32,
        height: i32,
    ) -> Result<([Frame; 2], ShmBuffer, PoolId)> {
        let (frames, mut memory, pool) =
            Frame::create_pair(sender, self.shm, width, height, Format::Xrgb8888)?;
        let pixels = self.pixels_for_size(width.cast_unsigned(), height.cast_unsigned());
        // Both slots receive identical pixels. The second buffer may be used
        // after a compositor release without asking the decoder to rerun.
        for slot in 0..2 {
            memory.pixels(slot).copy_from_slice(&pixels);
        }
        Ok((frames, memory, pool))
    }

    /// Render either the selected image or a solid-color frame.
    fn pixels_for_size(&self, width: u32, height: u32) -> Vec<u32> {
        self.wallpaper.as_ref().map_or_else(
            || vec![self.appearance.color.xrgb(); width as usize * height as usize],
            |wallpaper| wallpaper.render_xrgb(width, height, self.appearance.color.rgba()),
        )
    }

    /// Dispose the protocol resources for an obsolete configured size.
    fn destroy_frames(
        sender: &mut Sender,
        (frames, _memory, pool): ([Frame; 2], ShmBuffer, PoolId),
    ) -> Result<()> {
        for frame in frames {
            sender.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy))?;
        }
        sender.request(MessageBuilder::new(pool, ShmPoolRequest::Destroy))?;
        Ok(())
    }

    /// Attach the freshly rendered first slot and commit the replacement size.
    fn commit_first_frame(&self, sender: &mut Sender, width: i32, height: i32) -> Result<()> {
        let Some((frames, _, _)) = &self.frames else {
            return Ok(());
        };
        sender.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(frames[0].buffer)
                .i32(0)
                .i32(0),
        )?;
        sender.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(width)
                .i32(height),
        )?;
        sender.request(MessageBuilder::new(self.surface, SurfaceRequest::Commit))?;
        Ok(())
    }
}
