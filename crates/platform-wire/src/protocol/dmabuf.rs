use super::Interface;

pub const ZWP_LINUX_DMABUF_V1: Interface = Interface {
    name: "zwp_linux_dmabuf_v1",
    // v3 adds modifier event; v4 adds feedback (we skip for now).
    version: 3,
};

requests! {
    /// `zwp_linux_dmabuf_v1` requests — factory for dmabuf-based `wl_buffers`.
    DmabufRequest {
        /// Unbind the factory (created objects remain valid).
        Destroy = 0,
        /// Create a temporary params object for building a dmabuf buffer.
        CreateParams = 1,
    }
}
events! {
    /// `zwp_linux_dmabuf_v1` events.
    DmabufEvent {
        /// Advertised `DRM_FORMAT` code (deprecated in v4).
        Format = 0,
        /// Advertised modifier for a format (since v3, deprecated in v4).
        Modifier = 3,
    }
}

requests! {
    /// `zwp_linux_buffer_params_v1` requests — parameters for creating a
    /// dmabuf-based `wl_buffer`.
    DmabufParamsRequest {
        /// Delete this object, used or not.
        Destroy = 0,
        /// Add one dmabuf fd to the set. `fd` travels out-of-band via `SCM_RIGHTS`.
        Add = 1,
        /// Request asynchronous `wl_buffer` creation. New buffer id delivered via
        /// the `created` event.
        Create = 2,
        /// Request immediate `wl_buffer` creation. Pre-allocated `new_id`; no
        /// `created` event on success, `failed` on failure.
        CreateImmed = 3,
    }
}
events! {
    /// `zwp_linux_buffer_params_v1` events.
    DmabufParamsEvent {
        /// `Create` succeeded — carries the new `wl_buffer` id.
        Created = 0,
        /// Buffer creation failed (compositor can't import dmabuf).
        Failed = 1,
    }
}

/// DRM fourcc codes for dmabuf buffer creation.
pub mod drm_format {
    /// `DRM_FORMAT_XRGB8888` — 32-bit RGB, top byte unused.
    pub const XRGB8888: u32 = 0x3432_5258;
    /// `DRM_FORMAT_ARGB8888` — 32-bit ARGB, alpha in top byte.
    pub const ARGB8888: u32 = 0x3432_5241;
}

/// `DRM_FORMAT_MOD_LINEAR` — no tiling/compression.
pub const DRM_FORMAT_MOD_LINEAR_HI: u32 = 0;
pub const DRM_FORMAT_MOD_LINEAR_LO: u32 = 0;
