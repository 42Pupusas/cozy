use super::Interface;

pub const WL_SUBCOMPOSITOR: Interface = Interface {
    name: "wl_subcompositor",
    version: 1,
};

requests! {
    /// `wl_subcompositor` requests — gives a `wl_surface` the subsurface
    /// role, parented to another surface.
    SubcompositorRequest {
        Destroy = 0,
        /// Create a subsurface for `surface`, parented to `parent`.
        GetSubsurface = 1,
    }
}

requests! {
    /// `wl_subsurface` requests — positions and layers a child surface within
    /// its parent. Subsurfaces are a separate surface chain that commits
    /// independently or in sync with the parent.
    SubsurfaceRequest {
        Destroy = 0,
        /// Set the child's position relative to the parent, in surface-local
        /// pixels (before buffer scale).
        SetPosition = 1,
        /// Stack the child just above `sibling` in z-order.
        PlaceAbove = 2,
        /// Stack the child just below `sibling` in z-order.
        PlaceBelow = 3,
        /// Sync mode: the child's commits are cached until the parent commits.
        SetSync = 4,
        /// Desync mode: the child commits independently.
        SetDesync = 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn subsurface_opcodes_match_the_protocol_xml() {
        assert_eq!(SubcompositorRequest::Destroy.opcode(), 0);
        assert_eq!(SubcompositorRequest::GetSubsurface.opcode(), 1);
        assert_eq!(SubsurfaceRequest::Destroy.opcode(), 0);
        assert_eq!(SubsurfaceRequest::SetPosition.opcode(), 1);
        assert_eq!(SubsurfaceRequest::PlaceAbove.opcode(), 2);
        assert_eq!(SubsurfaceRequest::PlaceBelow.opcode(), 3);
        assert_eq!(SubsurfaceRequest::SetSync.opcode(), 4);
        assert_eq!(SubsurfaceRequest::SetDesync.opcode(), 5);
    }
}
