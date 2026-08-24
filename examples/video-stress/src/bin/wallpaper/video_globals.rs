//! The singleton globals a video background needs, plus the output list.

use platform::Result;
use platform::conn::Sender;
use platform::PlatformError;
use platform::id::{CompositorId, LayerShellId, OutputId, RegistryId, ShmId};
use platform::protocol as proto;
use platform::surface::{GlobalCollector, SurfaceBootstrap};

/// One bound `wl_output` and the registry name that announced it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VideoOutput {
    pub global_name: u32,
    pub id: OutputId,
}

/// The globals every video background surface shares.
pub struct VideoGlobals {
    pub compositor: CompositorId,
    pub shm: ShmId,
    pub layer_shell: LayerShellId,
}

/// Collects the singleton globals and defers output binding.
#[derive(Default)]
pub struct VideoGlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    layer_shell: Option<LayerShellId>,
    pub output_globals: Vec<(u32, u32)>,
}

impl VideoGlobalsBuilder {
    /// Take the collected singletons, failing when one never arrived.
    pub fn finish(&self) -> Result<VideoGlobals> {
        Ok(VideoGlobals {
            compositor: self.compositor.ok_or(PlatformError::ReaderStopped)?,
            shm: self.shm.ok_or(PlatformError::ReaderStopped)?,
            layer_shell: self.layer_shell.ok_or(PlatformError::ReaderStopped)?,
        })
    }
}

impl GlobalCollector for VideoGlobalsBuilder {
    fn is_complete(&self) -> bool {
        self.compositor.is_some() && self.shm.is_some() && self.layer_shell.is_some()
    }

    fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        name: u32,
        interface: &str,
        version: u32,
    ) -> Result<()> {
        match interface {
            found if found == proto::WL_COMPOSITOR.name => Self::bind_if_missing(
                &mut self.compositor,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_COMPOSITOR.version,
            ),
            found if found == proto::WL_SHM.name => Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            ),
            found if found == proto::ZWLR_LAYER_SHELL_V1.name => Self::bind_if_missing(
                &mut self.layer_shell,
                sender,
                registry,
                name,
                interface,
                version,
                proto::ZWLR_LAYER_SHELL_V1.version,
            ),
            found if found == proto::WL_OUTPUT.name => {
                self.output_globals.push((name, version));
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// The live outputs, keyed by the registry name that announced each one.
pub struct VideoOutputs {
    outputs: Vec<VideoOutput>,
}

impl VideoOutputs {
    /// Start with no outputs.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            outputs: Vec::new(),
        }
    }

    /// Bind one announced output, ignoring a duplicate announcement.
    pub fn bind_global(
        &mut self,
        sender: &mut Sender,
        registry: RegistryId,
        global_name: u32,
        advertised_version: u32,
    ) -> Result<Option<VideoOutput>> {
        if self
            .outputs
            .iter()
            .any(|output| output.global_name == global_name)
        {
            return Ok(None);
        }
        let version = proto::WL_OUTPUT.version.min(advertised_version);
        let id: OutputId = SurfaceBootstrap::bind(
            sender,
            registry,
            global_name,
            proto::WL_OUTPUT.name,
            version,
        )?;
        let output = VideoOutput { global_name, id };
        self.outputs.push(output);
        Ok(Some(output))
    }

    /// Forget the output announced under `global_name`.
    pub fn remove_global(&mut self, global_name: u32) -> Option<VideoOutput> {
        let index = self
            .outputs
            .iter()
            .position(|output| output.global_name == global_name)?;
        Some(self.outputs.remove(index))
    }

    /// Every currently live output.
    #[must_use]
    pub fn outputs(&self) -> &[VideoOutput] {
        &self.outputs
    }
}
