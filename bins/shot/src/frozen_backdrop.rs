//! Shows the pre-overlay capture underneath the selection dim.
//!
//! The capture's own `wl_buffer` is attached to a subsurface stacked below
//! the overlay, so the compositor displays the screen as it was before the
//! overlay took the pointer, and no pixel is copied on the client side.

use crate::backdrop_fit::BackdropFit;
use crate::overlay_globals::OverlayGlobals;
use platform::Result;
use platform::conn::Sender;
use platform::id::{SubsurfaceId, SurfaceId, ViewportId};
use platform::protocol::{
    CompositorRequest, SubcompositorRequest, SubsurfaceRequest, SurfaceRequest, ViewportRequest,
    ViewporterRequest,
};
use platform::wire::MessageBuilder;
use screencopy::HeldFrame;

/// The subsurface carrying the frozen screen.
pub struct FrozenBackdrop {
    surface: SurfaceId,
    subsurface: SubsurfaceId,
    viewport: Option<ViewportId>,
    fitted: Option<(i32, i32)>,
}

impl FrozenBackdrop {
    /// `wl_output.transform.flipped_180`: a vertical flip.
    const VERTICAL_FLIP: i32 = 6;

    /// Create the subsurface under `parent`, or `None` without a
    /// subcompositor.
    ///
    /// # Errors
    ///
    /// Propagates socket failures.
    pub fn create(
        sender: &mut Sender,
        globals: &OverlayGlobals,
        parent: SurfaceId,
    ) -> Result<Option<Self>> {
        let Some(subcompositor) = globals.subcompositor else {
            return Ok(None);
        };
        let surface: SurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(globals.compositor, CompositorRequest::CreateSurface)
                .new_id(surface),
        )?;
        let subsurface: SubsurfaceId = sender.alloc_id();
        sender.request(
            MessageBuilder::new(subcompositor, SubcompositorRequest::GetSubsurface)
                .new_id(subsurface)
                .object(surface)
                .object(parent),
        )?;
        sender.request(
            MessageBuilder::new(subsurface, SubsurfaceRequest::PlaceBelow).object(parent),
        )?;
        sender.request(MessageBuilder::new(
            subsurface,
            SubsurfaceRequest::SetDesync,
        ))?;
        let viewport = match globals.viewporter {
            Some(viewporter) => {
                let viewport: ViewportId = sender.alloc_id();
                sender.request(
                    MessageBuilder::new(viewporter, ViewporterRequest::GetViewport)
                        .new_id(viewport)
                        .object(surface),
                )?;
                Some(viewport)
            }
            None => None,
        };
        Ok(Some(Self {
            surface,
            subsurface,
            viewport,
            fitted: None,
        }))
    }

    /// Show `frame` stretched over a `surface`-sized overlay. A size that
    /// cannot be fitted leaves the backdrop unmapped and the live desktop
    /// visible, which only affects what is shown, not what is saved.
    ///
    /// # Errors
    ///
    /// Propagates socket failures.
    pub fn show(
        &mut self,
        sender: &mut Sender,
        frame: &HeldFrame<'_>,
        surface: (i32, i32),
    ) -> Result<()> {
        if self.fitted == Some(surface) {
            return Ok(());
        }
        let params = frame.view().params();
        let fit = BackdropFit::choose(
            (params.width, params.height),
            surface,
            self.viewport.is_some(),
        );
        let Some(fit) = fit else {
            return Ok(());
        };
        match (fit, self.viewport) {
            (BackdropFit::Viewport(width, height), Some(viewport)) => sender.request(
                MessageBuilder::new(viewport, ViewportRequest::SetDestination)
                    .i32(width)
                    .i32(height),
            )?,
            (BackdropFit::BufferScale(scale), _) => sender.request(
                MessageBuilder::new(self.surface, SurfaceRequest::SetBufferScale).i32(scale),
            )?,
            (BackdropFit::Viewport(..), None) => return Ok(()),
        }
        if frame.view().is_y_inverted() {
            sender.request(
                MessageBuilder::new(self.surface, SurfaceRequest::SetBufferTransform)
                    .i32(Self::VERTICAL_FLIP),
            )?;
        }
        sender.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                .object(frame.buffer())
                .i32(0)
                .i32(0),
        )?;
        sender.request(
            MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                .i32(0)
                .i32(0)
                .i32(i32::MAX)
                .i32(i32::MAX),
        )?;
        sender.request(MessageBuilder::new(self.surface, SurfaceRequest::Commit))?;
        self.fitted = Some(surface);
        Ok(())
    }

    /// Children first, then the surface, so the compositor drops its use of
    /// the capture buffer before the session may reuse it.
    ///
    /// # Errors
    ///
    /// Propagates socket failures.
    pub fn destroy(&self, sender: &mut Sender) -> Result<()> {
        if let Some(viewport) = self.viewport {
            sender.request(MessageBuilder::new(viewport, ViewportRequest::Destroy))?;
        }
        sender.request(MessageBuilder::new(
            self.subsurface,
            SubsurfaceRequest::Destroy,
        ))?;
        sender.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy))?;
        Ok(())
    }
}
