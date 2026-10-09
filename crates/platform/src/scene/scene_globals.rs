//! [`SceneGlobals`] -- the globals a window composed of sections binds.

use crate::conn::Sender;
use crate::error::{PlatformError, Result};
use crate::id::{
    CompositorId, OutputId, RegistryId, ShmId, SinglePixelBufferManagerId, SubcompositorId,
    ViewporterId, WmBaseId,
};
use crate::protocol as proto;
use crate::surface::GlobalCollector;

pub struct SceneGlobals {
    pub compositor: CompositorId,
    pub shm: ShmId,
    pub wm_base: WmBaseId,
    pub subcompositor: SubcompositorId,
    pub viewporter: ViewporterId,
    pub output: Option<OutputId>,
    /// Absent on niri; solid colours then come from 1×1 shm buffers.
    pub single_pixel: Option<SinglePixelBufferManagerId>,
}

/// Completes on the xdg-shell basics alone, so a compositor without the
/// section protocols fails in [`Self::finish`] with a named global instead
/// of blocking discovery forever.
#[derive(Default)]
pub struct SceneGlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    wm_base: Option<WmBaseId>,
    subcompositor: Option<SubcompositorId>,
    viewporter: Option<ViewporterId>,
    output: Option<OutputId>,
    single_pixel: Option<SinglePixelBufferManagerId>,
}

impl SceneGlobalsBuilder {
    pub fn finish(self) -> Result<SceneGlobals> {
        Ok(SceneGlobals {
            compositor: self
                .compositor
                .ok_or(PlatformError::MissingGlobal(proto::WL_COMPOSITOR.name))?,
            shm: self
                .shm
                .ok_or(PlatformError::MissingGlobal(proto::WL_SHM.name))?,
            wm_base: self
                .wm_base
                .ok_or(PlatformError::MissingGlobal(proto::XDG_WM_BASE.name))?,
            subcompositor: self
                .subcompositor
                .ok_or(PlatformError::MissingGlobal(proto::WL_SUBCOMPOSITOR.name))?,
            viewporter: self
                .viewporter
                .ok_or(PlatformError::MissingGlobal(proto::WP_VIEWPORTER.name))?,
            output: self.output,
            single_pixel: self.single_pixel,
        })
    }
}

impl GlobalCollector for SceneGlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.compositor.is_some() && self.shm.is_some() && self.wm_base.is_some()
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        let bind = |known: proto::Interface| (registry, name, interface, version, known.version);
        match interface {
            s if s == proto::WL_COMPOSITOR.name => {
                Self::bind_into(&mut self.compositor, sender, bind(proto::WL_COMPOSITOR))
            }
            s if s == proto::WL_SHM.name => {
                Self::bind_into(&mut self.shm, sender, bind(proto::WL_SHM))
            }
            s if s == proto::XDG_WM_BASE.name => {
                Self::bind_into(&mut self.wm_base, sender, bind(proto::XDG_WM_BASE))
            }
            s if s == proto::WL_SUBCOMPOSITOR.name => Self::bind_into(
                &mut self.subcompositor,
                sender,
                bind(proto::WL_SUBCOMPOSITOR),
            ),
            s if s == proto::WP_VIEWPORTER.name => {
                Self::bind_into(&mut self.viewporter, sender, bind(proto::WP_VIEWPORTER))
            }
            s if s == proto::WL_OUTPUT.name => {
                Self::bind_into(&mut self.output, sender, bind(proto::WL_OUTPUT))
            }
            s if s == proto::WP_SINGLE_PIXEL_BUFFER_MANAGER_V1.name => Self::bind_into(
                &mut self.single_pixel,
                sender,
                bind(proto::WP_SINGLE_PIXEL_BUFFER_MANAGER_V1),
            ),
            _ => Ok(()),
        }
    }
}

impl SceneGlobalsBuilder {
    fn bind_into<T: crate::id::ObjectId>(
        slot: &mut Option<T>,
        sender: &mut Sender,
        (registry, name, interface, advertised, known): (RegistryId, u32, &str, u32, u32),
    ) -> Result<()> {
        Self::bind_if_missing(slot, sender, registry, name, interface, advertised, known)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basics() -> SceneGlobalsBuilder {
        SceneGlobalsBuilder {
            compositor: Some(CompositorId(1)),
            shm: Some(ShmId(2)),
            wm_base: Some(WmBaseId(3)),
            ..Default::default()
        }
    }

    #[test]
    fn discovery_completes_without_the_section_protocols() {
        assert!(basics().is_complete());
        assert!(!SceneGlobalsBuilder::default().is_complete());
    }

    #[test]
    fn finish_names_the_missing_section_protocol() {
        let err = basics().finish().err().expect("subcompositor missing");
        assert!(matches!(
            err,
            PlatformError::MissingGlobal("wl_subcompositor")
        ));
        let mut b = basics();
        b.subcompositor = Some(SubcompositorId(4));
        let err = b.finish().err().expect("viewporter missing");
        assert!(matches!(err, PlatformError::MissingGlobal("wp_viewporter")));
    }

    #[test]
    fn single_pixel_buffers_are_optional() {
        let mut b = basics();
        b.subcompositor = Some(SubcompositorId(4));
        b.viewporter = Some(ViewporterId(5));
        let g = b.finish().expect("all required present");
        assert!(g.single_pixel.is_none());
    }
}
