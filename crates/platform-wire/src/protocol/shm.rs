use super::Interface;

pub const WL_SHM: Interface = Interface {
    name: "wl_shm",
    version: 1,
};

requests! {
    /// `wl_shm` requests — the shared-memory buffer factory.
    ShmRequest {
        CreatePool = 0,
    }
}
events! {
    /// `wl_shm` events.
    ShmEvent {
        /// The compositor advertises a supported pixel format.
        Format = 0,
    }
}

requests! {
    /// `wl_shm_pool` requests — carves buffers out of a shared-memory pool.
    ShmPoolRequest {
        CreateBuffer = 0,
        /// Destroy the pool and free the fd (v3+).
        Destroy = 1,
        /// Grow the pool to a new (larger) byte size after the backing fd has
        /// been `ftruncate`d. The compositor re-maps to match; the size may
        /// only ever increase.
        Resize = 2,
    }
}

requests! {
    /// `wl_buffer` requests.
    BufferRequest {
        /// Destroy the buffer object (its id becomes reusable once the server
        /// acknowledges with `wl_display.delete_id`).
        Destroy = 0,
    }
}
events! {
    /// `wl_buffer` events.
    BufferEvent {
        /// The compositor is done reading this buffer; we may reuse it.
        Release = 0,
    }
}

/// A `wl_shm` pixel format. Discriminant is the on-wire format code.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Format {
    /// 32-bit ARGB, alpha honoured.
    Argb8888 = 0,
    /// 32-bit RGB, the high byte ignored. What we draw into.
    Xrgb8888 = 1,
}

impl Format {
    #[must_use]
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Decode a wire format code, or `None` for a format we don't model.
    ///
    /// Needed because some protocols *tell us* which format to use rather
    /// than letting us pick: `zwlr_screencopy_frame_v1.buffer` names the
    /// format the compositor will copy into, so a capture client has to turn
    /// that `uint` back into something it can allocate and interpret.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            0 => Some(Self::Argb8888),
            1 => Some(Self::Xrgb8888),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_code_matches_wire_value() {
        assert_eq!(Format::Argb8888.code(), 0);
        assert_eq!(Format::Xrgb8888.code(), 1);
    }

    #[test]
    fn format_from_code_roundtrips_and_rejects_unknown() {
        for f in [Format::Argb8888, Format::Xrgb8888] {
            assert_eq!(Format::from_code(f.code()), Some(f));
        }
        for code in [2, 3, 0x3432_5258, u32::MAX] {
            assert_eq!(
                Format::from_code(code),
                None,
                "code {code} should not decode"
            );
        }
    }
}
