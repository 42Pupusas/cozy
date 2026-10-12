//! The globals the selection overlay draws with.

use platform::id::{CompositorId, LayerShellId, ShmId, SubcompositorId, ViewporterId};

/// Factories for the overlay's surfaces. The subcompositor and viewporter are
/// optional: without a subcompositor there is no frozen backdrop, and without
/// a viewporter it only fits integer scales.
#[derive(Copy, Clone)]
pub struct OverlayGlobals {
    pub compositor: CompositorId,
    pub layer_shell: LayerShellId,
    pub shm: ShmId,
    pub subcompositor: Option<SubcompositorId>,
    pub viewporter: Option<ViewporterId>,
}
