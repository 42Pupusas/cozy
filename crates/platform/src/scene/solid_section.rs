//! [`SolidSection`] -- a surface showing a single colour, sized by its
//! viewport. A child section is a subsurface; the root section is the
//! window's own surface, which its owner commits.

use crate::conn::WindowSender;
use crate::id::{
    BufferId, CompositorId, RegionId, SubcompositorId, SubsurfaceId, SurfaceId, ViewportId,
    ViewporterId,
};
use crate::protocol::{
    CompositorRequest, SubcompositorRequest, SubsurfaceRequest, SurfaceRequest, ViewportRequest,
    ViewporterRequest,
};
use crate::wire::MessageBuilder;

use super::section_state::{SectionChange, SectionState};

/// The factories and parent a new child section is created against.
#[derive(Clone, Copy)]
pub struct SectionFactory {
    pub compositor: CompositorId,
    pub subcompositor: SubcompositorId,
    pub viewporter: ViewporterId,
    pub parent: SurfaceId,
    /// Empty region, so sections never take pointer input from the parent.
    pub no_input: RegionId,
}

pub struct SolidSection {
    surface: SurfaceId,
    subsurface: Option<SubsurfaceId>,
    viewport: ViewportId,
    shown: Option<SectionState>,
}

impl SolidSection {
    /// A child stacked above every section created before it.
    pub fn child(window_tx: &mut WindowSender, factory: SectionFactory) -> Self {
        let surface: SurfaceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(factory.compositor, CompositorRequest::CreateSurface)
                .new_id(surface),
        );
        let subsurface: SubsurfaceId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(factory.subcompositor, SubcompositorRequest::GetSubsurface)
                .new_id(subsurface)
                .object(surface)
                .object(factory.parent),
        );
        window_tx.request(
            MessageBuilder::new(surface, SurfaceRequest::SetInputRegion).object(factory.no_input),
        );
        let mut section = Self::over(window_tx, factory.viewporter, surface);
        section.subsurface = Some(subsurface);
        section
    }

    /// The window's own surface as a section. A 1×1 buffer is not a multiple
    /// of a `HiDPI` scale, so the surface goes back to scale 1; the viewport
    /// sets its size.
    pub fn root(
        window_tx: &mut WindowSender,
        viewporter: ViewporterId,
        surface: SurfaceId,
    ) -> Self {
        window_tx.request(MessageBuilder::new(surface, SurfaceRequest::SetBufferScale).i32(1));
        Self::over(window_tx, viewporter, surface)
    }

    fn over(window_tx: &mut WindowSender, viewporter: ViewporterId, surface: SurfaceId) -> Self {
        let viewport: ViewportId = window_tx.alloc_id();
        window_tx.request(
            MessageBuilder::new(viewporter, ViewporterRequest::GetViewport)
                .new_id(viewport)
                .object(surface),
        );
        Self {
            surface,
            subsurface: None,
            viewport,
            shown: None,
        }
    }

    /// Send what changed since the last call. `buffer` is asked for only when
    /// the colour changed. Returns how many requests went out.
    pub fn apply(
        &mut self,
        window_tx: &WindowSender,
        next: SectionState,
        buffer: impl FnOnce(u32) -> Option<BufferId>,
    ) -> usize {
        let change = next.change_from(self.shown.as_ref());
        self.shown = Some(next);
        if change.is_empty() {
            return 0;
        }
        self.send(window_tx, change, buffer)
    }

    fn send(
        &self,
        window_tx: &WindowSender,
        change: SectionChange,
        buffer: impl FnOnce(u32) -> Option<BufferId>,
    ) -> usize {
        let mut sent = 0;
        if let (Some((x, y)), Some(subsurface)) = (change.position, self.subsurface) {
            window_tx.request(
                MessageBuilder::new(subsurface, SubsurfaceRequest::SetPosition)
                    .i32(x)
                    .i32(y),
            );
            sent += 1;
        }
        if change.hide {
            window_tx.request(
                MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                    .u32(0)
                    .i32(0)
                    .i32(0),
            );
            sent += 1;
        }
        if let Some((w, h)) = change.size {
            window_tx.request(
                MessageBuilder::new(self.viewport, ViewportRequest::SetDestination)
                    .i32(w)
                    .i32(h),
            );
            sent += 1;
        }
        if let Some(buffer) = change.color.and_then(buffer) {
            window_tx.request(
                MessageBuilder::new(self.surface, SurfaceRequest::Attach)
                    .object(buffer)
                    .i32(0)
                    .i32(0),
            );
            window_tx.request(
                MessageBuilder::new(self.surface, SurfaceRequest::Damage)
                    .i32(0)
                    .i32(0)
                    .i32(i32::MAX)
                    .i32(i32::MAX),
            );
            sent += 2;
        }
        if self.subsurface.is_some() && change.needs_commit() {
            window_tx.request(MessageBuilder::new(self.surface, SurfaceRequest::Commit));
            sent += 1;
        }
        sent
    }

    /// Child objects first, then the surface they belong to. The root's
    /// surface belongs to the shell, so only its viewport goes.
    pub fn destroy(&self, window_tx: &WindowSender) {
        window_tx.request(MessageBuilder::new(self.viewport, ViewportRequest::Destroy));
        if let Some(subsurface) = self.subsurface {
            window_tx.request(MessageBuilder::new(subsurface, SubsurfaceRequest::Destroy));
            window_tx.request(MessageBuilder::new(self.surface, SurfaceRequest::Destroy));
        }
    }
}
