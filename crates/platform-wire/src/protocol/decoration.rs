use super::Interface;

pub const ZXDG_DECORATION_MANAGER_V1: Interface = Interface {
    name: "zxdg_decoration_manager_v1",
    version: 1,
};

requests! {
    /// `zxdg_decoration_manager_v1` requests — the SSD factory.
    DecorationManagerRequest {
        /// Create a decoration object for a toplevel, then `set_mode` on it.
        GetToplevelDecoration = 1,
    }
}

requests! {
    /// `zxdg_toplevel_decoration_v1` requests.
    ToplevelDecorationRequest {
        SetMode = 1,
    }
}

/// `zxdg_toplevel_decoration_v1` mode values (argument to `set_mode`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DecorationMode {
    /// Client draws its own decorations.
    ClientSide = 1,
    /// Compositor draws the titlebar/border — what we want.
    ServerSide = 2,
}
