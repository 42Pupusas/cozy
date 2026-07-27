//! Typed Wayland object ids.
//!
//! Every Wayland object is identified on the wire by a `u32`. Using a bare
//! `u32` everywhere invites bugs — nothing stops you handing a buffer id where a
//! surface id is expected. Instead each interface gets its own newtype, so the
//! compiler enforces that, say, a [`SurfaceId`] and a [`BufferId`] never get
//! mixed up.
//!
//! All ids implement [`ObjectId`], which exposes the raw `u32` for the few
//! places that genuinely need it (wire encoding, comparing an incoming event's
//! object id). [`MessageBuilder::new`](crate::wire::MessageBuilder::new) accepts
//! any `impl ObjectId`, and the connection's allocator hands back whichever id
//! type the call site asks for.

/// A Wayland object id: a newtype over the wire `u32`.
pub trait ObjectId: Copy {
    /// The raw wire value.
    fn raw(self) -> u32;
    /// Wrap a raw wire value.
    fn from_raw(raw: u32) -> Self;
}

/// Declare one or more object-id newtypes implementing [`ObjectId`].
macro_rules! object_ids {
    ($($(#[$m:meta])* $name:ident),+ $(,)?) => {
        $(
            $(#[$m])*
            #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
            pub struct $name(pub u32);

            impl ObjectId for $name {
                #[inline]
                fn raw(self) -> u32 {
                    self.0
                }
                #[inline]
                fn from_raw(raw: u32) -> Self {
                    Self(raw)
                }
            }
        )+
    };
}

object_ids! {
    /// `wl_display` — always id 1, the root object.
    DisplayId,
    /// `wl_registry` — the global registry.
    RegistryId,
    /// `wl_compositor` — surface factory.
    CompositorId,
    /// `wl_shm` — shared-memory buffer factory.
    ShmId,
    /// `wl_shm_pool` — a pool buffers are carved from.
    PoolId,
    /// `wl_buffer` — one attachable pixel buffer.
    BufferId,
    /// `wl_surface` — a composited rectangle of pixels.
    SurfaceId,
    /// `wl_seat` — an input-device group.
    SeatId,
    /// `wl_pointer` — a pointer device.
    PointerId,
    /// `wl_keyboard` — a keyboard device.
    KeyboardId,
    /// `wl_callback` — a one-shot callback (used for frame throttling).
    CallbackId,
    /// `xdg_wm_base` — the xdg-shell global.
    WmBaseId,
    /// `xdg_surface` — bridges a `wl_surface` to xdg-shell roles.
    XdgSurfaceId,
    /// `xdg_toplevel` — a top-level application window.
    ToplevelId,
    /// `zwp_linux_dmabuf_v1` — the dmabuf factory global.
    DmabufId,
    /// `zwp_linux_buffer_params_v1` — temporary object for dmabuf buffer creation.
    DmabufParamsId,
    /// `zxdg_decoration_manager_v1` — server-side-decoration factory global.
    DecorationManagerId,
    /// `zxdg_toplevel_decoration_v1` — per-toplevel decoration object.
    ToplevelDecorationId,
    /// `wl_output` — a physical/logical display, advertising its scale factor.
    OutputId,
    /// `wl_data_device_manager` — clipboard/DnD factory global.
    DataDeviceManagerId,
    /// `wl_data_device` — per-seat clipboard/DnD endpoint.
    DataDeviceId,
    /// `wl_data_source` — data we offer to other clients (our copy).
    DataSourceId,
    /// `wl_data_offer` — data another client offers us (their copy / our paste).
    DataOfferId,
    /// `zwlr_layer_shell_v1` — the layer-surface factory global.
    LayerShellId,
    /// `zwlr_layer_surface_v1` — an edge-anchored surface (bars, panels).
    LayerSurfaceId,
    /// `zwlr_screencopy_manager_v1` — the screen-capture factory global.
    ScreencopyManagerId,
    /// `zwlr_screencopy_frame_v1` — one single-use capture of an output.
    ScreencopyFrameId,
}

/// An event serial — echoed back in acks/pongs. Distinct from an object id so
/// the two can't be confused.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Serial(pub u32);

impl Serial {
    #[inline]
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}
