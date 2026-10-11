//! The `zwlr_layer_shell_v1` surface lifecycle shared by every layer-shell
//! window in the workspace.
//!
//! # Why this exists
//!
//! Four independent windows each hand-rolled the same object chain and
//! configure/destroy handshake: `statusbar::BarWindow`,
//! `launcher::LauncherWindow`, `wallpaper::BackgroundSurface`, and
//! `shot::SelectionOverlay`. All four do exactly the same sequence --
//! create a `wl_surface`, optionally declare its buffer scale, turn it into
//! a layer surface, apply whichever subset of anchor/size/exclusive-zone/
//! margin/keyboard-interactivity that role needs, and commit a buffer-less
//! initial frame to solicit the first `configure` -- then later `ack` that
//! configure and, on teardown, destroy the layer surface before the plain
//! surface beneath it (destroying in the other order is a protocol error,
//! since the layer surface is a child role object).
//!
//! That handshake is genuinely identical across roles; only the *values*
//! differ (a bar anchors and reserves space, a launcher centers and takes
//! no space, a background/picker anchors all four edges and reserves
//! none). [`LayerSurfaceConfig`] captures exactly that per-role variation;
//! everything else lives here once.
//!
//! This is the layer-shell sibling of [`crate::toplevel::shell::ShellSurface`],
//! which does the analogous job for the xdg-shell toplevel role.

use crate::conn::{Sender, WindowSender};
use crate::error::Result;
use crate::id::{CompositorId, LayerShellId, LayerSurfaceId, OutputId, SurfaceId};
use crate::protocol::{
    CompositorRequest, KeyboardInteractivity, Layer, LayerShellRequest, LayerSurfaceRequest,
    SurfaceRequest,
};
use crate::wire::MessageBuilder;

/// The role-specific choices one layer-shell window makes at creation.
///
/// Every field a role doesn't need is `None` (or, for `output`, the
/// `OutputId(0)` sentinel meaning "let the compositor pick") rather than a
/// zero value that would still issue the request -- e.g. a launcher that
/// wants no anchor at all must skip `set_anchor` entirely, not send it with
/// an empty bitmask, since the two are not obviously equivalent across
/// compositors and the existing four windows never sent the request they
/// didn't need.
pub struct LayerSurfaceConfig<'a> {
    /// Compositor stacking layer (background/bottom/top/overlay).
    pub layer: Layer,
    /// Identifies this client in `get_layer_surface`, e.g. `"statusbar"`.
    pub namespace: &'a str,
    /// Which output to anchor to, or `OutputId(0)` to let the compositor
    /// pick (the two windows that don't target a specific output).
    pub output: OutputId,
    /// `wl_surface.set_buffer_scale`, sent before the layer surface is
    /// created. `None` for the two windows that never probed a scale up
    /// front and just accept whatever the first configure reports.
    pub buffer_scale: Option<i32>,
    /// `zwlr_layer_surface_v1.set_anchor` bits (see [`crate::protocol::Anchor::bits`]).
    /// `None` skips the request, which is what centers an unanchored
    /// surface on the output.
    pub anchor: Option<u32>,
    /// `zwlr_layer_surface_v1.set_size`. `None` skips the request, correct
    /// when all four edges are anchored and the compositor decides.
    pub size: Option<(i32, i32)>,
    /// `zwlr_layer_surface_v1.set_exclusive_zone`. `None` skips the
    /// request (a launcher reserves no space and doesn't ask the
    /// compositor to keep any clear either).
    pub exclusive_zone: Option<i32>,
    /// `zwlr_layer_surface_v1.set_margin` as `(top, right, bottom, left)`.
    /// `None` skips the request.
    pub margin: Option<(i32, i32, i32, i32)>,
    /// `zwlr_layer_surface_v1.set_keyboard_interactivity`. Always sent --
    /// every existing role has an opinion here, even if that opinion is
    /// [`KeyboardInteractivity::None`].
    pub keyboard_interactivity: KeyboardInteractivity,
}

/// One live `wl_surface` + `zwlr_layer_surface_v1` pair.
///
/// Deliberately holds nothing beyond the two ids: buffers, painters, and
/// event routing stay with the owning window, exactly as
/// [`crate::toplevel::shell::ShellSurface`] holds only the xdg-shell object
/// chain and nothing content-related.
#[derive(Debug, Clone, Copy)]
pub struct LayerSurfaceHandle {
    pub surface: SurfaceId,
    pub layer_surface: LayerSurfaceId,
}

impl LayerSurfaceHandle {
    /// Create the `wl_surface` → `zwlr_layer_surface_v1` chain, apply
    /// `config`, and issue the buffer-less initial commit that solicits the
    /// first `configure`.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn create(
        sender: &mut Sender,
        compositor: CompositorId,
        layer_shell: LayerShellId,
        config: &LayerSurfaceConfig<'_>,
    ) -> Result<Self> {
        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(compositor, CompositorRequest::CreateSurface).new_id(surface),
        )?;
        if let Some(scale) = config.buffer_scale {
            sender
                .request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(scale))?;
        }

        let layer_surface: LayerSurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(layer_shell, LayerShellRequest::GetLayerSurface)
                .new_id(layer_surface)
                .object(surface)
                .object(config.output)
                .u32(config.layer as u32)
                .string(config.namespace),
        )?;
        if let Some(bits) = config.anchor {
            sender.request(
                MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetAnchor).u32(bits),
            )?;
        }
        if let Some((w, h)) = config.size {
            sender.request(
                MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetSize)
                    .i32(w)
                    .i32(h),
            )?;
        }
        if let Some(zone) = config.exclusive_zone {
            sender.request(
                MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetExclusiveZone).i32(zone),
            )?;
        }
        if let Some((top, right, bottom, left)) = config.margin {
            sender.request(
                MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetMargin)
                    .i32(top)
                    .i32(right)
                    .i32(bottom)
                    .i32(left),
            )?;
        }
        sender.request(
            MessageBuilder::new(layer_surface, LayerSurfaceRequest::SetKeyboardInteractivity)
                .u32(config.keyboard_interactivity as u32),
        )?;
        // The buffer-less commit that provokes the first configure -- same
        // contract as `ShellSurface::create`'s matching commit.
        sender.request(MessageBuilder::new(surface, SurfaceRequest::Commit))?;

        Ok(Self {
            surface,
            layer_surface,
        })
    }

    /// Acknowledge a `configure` from the main connection, for windows
    /// (wallpaper, shot) that drive their event loop directly off `Sender`
    /// rather than through a queued [`WindowSender`].
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures.
    pub fn ack_configure(&self, sender: &mut Sender, serial: u32) -> Result<()> {
        Ok(sender.request(
            MessageBuilder::new(self.layer_surface, LayerSurfaceRequest::AckConfigure).u32(serial),
        )?)
    }

    /// Acknowledge a `configure` from inside event dispatch, for windows
    /// (bar, launcher) whose real connection is borrowed by the event loop
    /// and which queue requests through a [`WindowSender`] instead.
    pub fn ack_configure_queued(&self, window_tx: &WindowSender, serial: u32) {
        window_tx.request(
            MessageBuilder::new(self.layer_surface, LayerSurfaceRequest::AckConfigure).u32(serial),
        );
    }

    /// Change keyboard interactivity from inside event dispatch; takes
    /// effect on the surface's next commit.
    pub fn set_keyboard_interactivity_queued(
        &self,
        window_tx: &WindowSender,
        mode: KeyboardInteractivity,
    ) {
        window_tx.request(
            MessageBuilder::new(
                self.layer_surface,
                LayerSurfaceRequest::SetKeyboardInteractivity,
            )
            .u32(mode as u32),
        );
    }

    /// Tear the chain down in the reverse order it was built: the layer
    /// surface is a child role object of the plain surface, so destroying
    /// the surface first is a protocol error.
    ///
    /// # Errors
    ///
    /// Propagates protocol/socket failures. Callers tearing down from a
    /// `Drop` impl (which cannot propagate) discard the result, matching
    /// what the four call sites already did before this type existed.
    pub fn destroy(&self, sender: &mut Sender) -> Result<()> {
        sender.request(MessageBuilder::new(
            self.layer_surface,
            LayerSurfaceRequest::Destroy,
        ))?;
        sender.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy))?;
        Ok(())
    }
}
