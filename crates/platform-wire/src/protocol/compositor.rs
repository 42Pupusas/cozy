use super::Interface;

pub const WL_COMPOSITOR: Interface = Interface {
    name: "wl_compositor",
    version: 4,
};

requests! {
    /// `wl_compositor` requests — the factory for surfaces.
    CompositorRequest {
        CreateSurface = 0,
        CreateRegion = 1,
    }
}

requests! {
    /// `wl_surface` requests — a rectangle of pixels the compositor composites.
    SurfaceRequest {
        Destroy = 0,
        Attach = 1,
        Damage = 2,
        /// Request a one-shot frame-throttling callback (a `wl_callback` whose
        /// `done` event fires when the compositor is ready for the next frame).
        Frame = 3,
        SetOpaqueRegion = 4,
        SetInputRegion = 5,
        Commit = 6,
        /// Set the buffer scale (integer, ≥1). 1 = 1:1, 2 = `HiDPI` @ 2x.
        SetBufferScale = 8,
        DamageBuffer = 9,
    }
}

requests! {
    /// `wl_region` requests.
    RegionRequest {
        Destroy = 0,
        Add = 1,
        Subtract = 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn surface_request_opcodes_match_the_protocol_xml() {
        assert_eq!(SurfaceRequest::Destroy.opcode(), 0);
        assert_eq!(SurfaceRequest::Attach.opcode(), 1);
        assert_eq!(SurfaceRequest::Damage.opcode(), 2);
        assert_eq!(SurfaceRequest::Frame.opcode(), 3);
        assert_eq!(SurfaceRequest::SetOpaqueRegion.opcode(), 4);
        assert_eq!(SurfaceRequest::SetInputRegion.opcode(), 5);
        assert_eq!(SurfaceRequest::Commit.opcode(), 6);
        assert_eq!(SurfaceRequest::SetBufferScale.opcode(), 8);
        assert_eq!(SurfaceRequest::DamageBuffer.opcode(), 9);
    }

    #[test]
    fn region_opcodes_match_the_protocol_xml() {
        assert_eq!(CompositorRequest::CreateSurface.opcode(), 0);
        assert_eq!(CompositorRequest::CreateRegion.opcode(), 1);
        assert_eq!(RegionRequest::Destroy.opcode(), 0);
        assert_eq!(RegionRequest::Add.opcode(), 1);
        assert_eq!(RegionRequest::Subtract.opcode(), 2);
    }
}
