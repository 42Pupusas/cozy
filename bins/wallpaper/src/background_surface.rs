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
use platform::id::{CompositorId, LayerShellId, OutputId, PoolId, ShmId};
use platform::layer::{LayerSurfaceConfig, LayerSurfaceHandle};
use platform::protocol::{
    Anchor, BufferRequest, Format, KeyboardInteractivity, Layer, LayerSurfaceEvent,
    ShmPoolRequest, SurfaceRequest,
};
use platform::surface::Frame;
use platform::sys::ShmBuffer;
use platform::wire::MessageBuilder;

const APP_ID: &str = "wallpaper";

/// The objects and pixels for one compositor output's background.
pub struct BackgroundSurface {
    output: OutputId,
    shm: ShmId,
    layer: LayerSurfaceHandle,
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
        // `-1` is the layer-shell sentinel for a true full-output surface:
        // do not reserve usable space and do not be inset by another layer's
        // exclusive zone (such as a status bar). `0` merely reserves nothing;
        // it still permits the compositor to size this surface around a bar.
        // The initial buffer-less commit inside `create` asks layer-shell for
        // the output dimensions.
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
        let mut background = Self {
            output,
            shm,
            layer,
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
        self.layer.destroy(sender)?;
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
        if !event.is(self.layer.layer_surface)
            || event.event::<LayerSurfaceEvent>() != Some(LayerSurfaceEvent::Configure)
        {
            return Ok(());
        }
        self.layer.ack_configure(sender, event.arg(0))?;
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
        let width = width.cast_unsigned();
        let height = height.cast_unsigned();
        self.render_pixels_into(&mut memory, width, height);
        // Both slots receive identical pixels. The second buffer may be used
        // after a compositor release without asking the decoder to rerun.
        let first = memory.pixels(0).to_owned();
        memory.pixels(1).copy_from_slice(&first);
        Ok((frames, memory, pool))
    }

    /// Render directly into the first shm slot, avoiding a temporary XRGB
    /// vector in addition to the wallpaper decoder's reusable RGBA scratch.
    fn render_pixels_into(&self, memory: &mut ShmBuffer, width: u32, height: u32) {
        let pad = self.appearance.color.rgba();
        if let Some(wallpaper) = &self.wallpaper {
            wallpaper.render_xrgb_into(width, height, pad, memory.pixels(0));
        } else {
            memory.pixels(0).fill(self.appearance.color.xrgb());
        }
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
            MessageBuilder::new(self.layer.surface, SurfaceRequest::Attach)
                .object(frames[0].buffer)
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
        sender.request(MessageBuilder::new(self.layer.surface, SurfaceRequest::Commit))?;
        Ok(())
    }
}
