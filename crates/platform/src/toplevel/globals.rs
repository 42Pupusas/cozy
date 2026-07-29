//! Registry discovery for a generic application window.
//!
//! Its own module because the *set* of globals a role needs is exactly what
//! distinguishes one shell role from another -- it is the role's contract
//! with the compositor. `TermWindow` also wants `wl_data_device_manager`
//! (clipboard); a layer-shell bar wants `zwlr_layer_shell_v1` instead of
//! `xdg_wm_base`. Keeping each role's list in its own file means adding an
//! optional global here can't perturb the terminal's startup.

use crate::conn::Sender;
use crate::error::Result;
use crate::id::{
    CompositorId, DecorationManagerId, OutputId, RegistryId, SeatId, ShmId, WmBaseId,
};
use crate::protocol as proto;
use crate::surface::GlobalCollector;

/// The globals a toplevel window binds, once discovery has completed.
///
/// `output` and `decoration_manager` are optional on purpose -- see
/// [`ToplevelGlobals::is_complete`].
pub struct ToplevelGlobals {
    pub compositor: CompositorId,
    pub shm: ShmId,
    pub wm_base: WmBaseId,
    pub seat: SeatId,
    /// Used only to probe the buffer scale at startup. Some nested and
    /// headless compositors advertise no output at all.
    pub output: Option<OutputId>,
    /// `zxdg_decoration_manager_v1`. When present we ask for *server-side*
    /// decorations, so the window gets the compositor's own titlebar and
    /// matches every other window on screen. When absent (GNOME, notably)
    /// the window is simply undecorated -- which is why this is optional
    /// rather than required: refusing to start without a titlebar would be
    /// absurd.
    pub decoration_manager: Option<DecorationManagerId>,
}

/// Accumulates globals as `wl_registry.global` events arrive.
#[derive(Default)]
pub struct ToplevelGlobalsBuilder {
    compositor: Option<CompositorId>,
    shm: Option<ShmId>,
    wm_base: Option<WmBaseId>,
    seat: Option<SeatId>,
    output: Option<OutputId>,
    decoration_manager: Option<DecorationManagerId>,
}

impl ToplevelGlobalsBuilder {
    /// Consume the builder into the discovered set.
    ///
    /// Returns `None` unless every required global was bound, so a caller
    /// cannot accidentally proceed with a half-built window.
    #[must_use]
    pub fn finish(self) -> Option<ToplevelGlobals> {
        Some(ToplevelGlobals {
            compositor: self.compositor?,
            shm: self.shm?,
            wm_base: self.wm_base?,
            seat: self.seat?,
            output: self.output,
            decoration_manager: self.decoration_manager,
        })
    }
}

impl GlobalCollector for ToplevelGlobalsBuilder {
    /// Whether every *required* global has arrived.
    ///
    /// `wl_output` and the decoration manager are excluded: the discovery
    /// loop stops as soon as this returns true, so optional globals get only
    /// whatever chance they had to appear earlier in the same unordered
    /// burst. That is acceptable for both -- a missing output means we
    /// assume scale 1, a missing decoration manager means no titlebar.
    fn is_complete(&self) -> bool {
        self.compositor.is_some()
            && self.shm.is_some()
            && self.wm_base.is_some()
            && self.seat.is_some()
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
            s if s == proto::WL_COMPOSITOR.name => Self::bind_if_missing(
                &mut self.compositor,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_COMPOSITOR.version,
            ),
            s if s == proto::WL_SHM.name => Self::bind_if_missing(
                &mut self.shm,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SHM.version,
            ),
            s if s == proto::XDG_WM_BASE.name => Self::bind_if_missing(
                &mut self.wm_base,
                sender,
                registry,
                name,
                interface,
                version,
                proto::XDG_WM_BASE.version,
            ),
            s if s == proto::WL_SEAT.name => Self::bind_if_missing(
                &mut self.seat,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_SEAT.version,
            ),
            s if s == proto::WL_OUTPUT.name => Self::bind_if_missing(
                &mut self.output,
                sender,
                registry,
                name,
                interface,
                version,
                proto::WL_OUTPUT.version,
            ),
            s if s == proto::ZXDG_DECORATION_MANAGER_V1.name => Self::bind_if_missing(
                &mut self.decoration_manager,
                sender,
                registry,
                name,
                interface,
                version,
                proto::ZXDG_DECORATION_MANAGER_V1.version,
            ),
            // Compositors advertise dozens of globals we have no use for.
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The completeness rule is pure logic, so it is testable without a
    /// compositor -- which is the point of keeping it on the builder rather
    /// than inline in a discovery loop.
    #[test]
    fn an_empty_builder_is_not_complete() {
        assert!(!ToplevelGlobalsBuilder::default().is_complete());
    }

    #[test]
    fn all_four_required_globals_make_it_complete() {
        let b = ToplevelGlobalsBuilder {
            compositor: Some(CompositorId(1)),
            shm: Some(ShmId(2)),
            wm_base: Some(WmBaseId(3)),
            seat: Some(SeatId(4)),
            output: None,
            decoration_manager: None,
        };
        assert!(b.is_complete());
    }

    #[test]
    fn optional_globals_alone_do_not_complete_discovery() {
        // Otherwise startup would race ahead before the compositor is bound.
        let b = ToplevelGlobalsBuilder {
            output: Some(OutputId(9)),
            decoration_manager: Some(DecorationManagerId(10)),
            ..Default::default()
        };
        assert!(!b.is_complete());
    }

    #[test]
    fn a_missing_required_global_blocks_completion() {
        // Each of the four is individually load-bearing.
        let full = || ToplevelGlobalsBuilder {
            compositor: Some(CompositorId(1)),
            shm: Some(ShmId(2)),
            wm_base: Some(WmBaseId(3)),
            seat: Some(SeatId(4)),
            output: None,
            decoration_manager: None,
        };
        let mut b = full();
        b.compositor = None;
        assert!(!b.is_complete());
        let mut b = full();
        b.shm = None;
        assert!(!b.is_complete());
        let mut b = full();
        b.wm_base = None;
        assert!(!b.is_complete());
        let mut b = full();
        b.seat = None;
        assert!(!b.is_complete());
    }

    #[test]
    fn finish_refuses_to_build_a_half_discovered_window() {
        assert!(ToplevelGlobalsBuilder::default().finish().is_none());
    }

    #[test]
    fn finish_succeeds_without_the_optional_globals() {
        // A compositor with no decoration manager (GNOME) must still start.
        let b = ToplevelGlobalsBuilder {
            compositor: Some(CompositorId(1)),
            shm: Some(ShmId(2)),
            wm_base: Some(WmBaseId(3)),
            seat: Some(SeatId(4)),
            output: None,
            decoration_manager: None,
        };
        let g = b.finish().expect("required globals present");
        assert!(g.output.is_none());
        assert!(g.decoration_manager.is_none());
    }
}
