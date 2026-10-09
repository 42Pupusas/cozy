use super::Interface;

pub const XDG_WM_BASE: Interface = Interface {
    name: "xdg_wm_base",
    version: 1,
};

requests! {
    /// `xdg_wm_base` requests — the shell that turns surfaces into windows.
    WmBaseRequest {
        GetXdgSurface = 2,
        Pong = 3,
    }
}
events! {
    /// `xdg_wm_base` events.
    WmBaseEvent {
        /// Liveness check; we must `Pong` with the same serial or get killed.
        Ping = 0,
    }
}

requests! {
    /// `xdg_surface` requests — bridges a `wl_surface` to xdg-shell roles.
    XdgSurfaceRequest {
        Destroy = 0,
        GetToplevel = 1,
        AckConfigure = 4,
    }
}
events! {
    /// `xdg_surface` events.
    XdgSurfaceEvent {
        /// The compositor proposes a configuration; ack it, then commit.
        Configure = 0,
    }
}

requests! {
    /// `xdg_toplevel` requests — a top-level application window.
    ToplevelRequest {
        Destroy = 0,
        SetTitle = 2,
        /// Stable, machine-facing identifier (`app_id`) the compositor uses for
        /// window rules / matching — distinct from the human-facing title,
        /// which apps overwrite at runtime via OSC.
        SetAppId = 3,
    }
}
events! {
    /// `xdg_toplevel` events.
    ToplevelEvent {
        /// Suggested new size and states.
        Configure = 0,
        /// The user (or compositor) asked the window to close.
        Close = 1,
    }
}
