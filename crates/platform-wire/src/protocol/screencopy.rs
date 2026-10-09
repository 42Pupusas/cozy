//! `zwlr_screencopy_v1`: a client asks the compositor for a copy of an
//! output. Each capture is a single-use frame object: the compositor names
//! the buffer to allocate (`buffer`, then `buffer_done` on v3), the client
//! sends `copy`, and the frame answers with `flags` + `ready` or `failed`.
//! The compositor dictates the format and stride.

use super::Interface;

pub const ZWLR_SCREENCOPY_MANAGER_V1: Interface = Interface {
    name: "zwlr_screencopy_manager_v1",
    // v3 adds `buffer_done`, which marks the end of the buffer-parameter
    // events; without it a client has to assume one `buffer` event is all.
    version: 3,
};

requests! {
    /// `zwlr_screencopy_manager_v1` requests — the factory for capture frames.
    ScreencopyManagerRequest {
        /// Capture a whole output: `frame` (`new_id`), `overlay_cursor`
        /// (int, non-zero composites the cursor into the copy), `output`.
        CaptureOutput = 0,
        /// Capture a sub-rectangle of an output: `frame` (`new_id`),
        /// `overlay_cursor`, `output`, `x`, `y`, `width`, `height`, the box
        /// given in output-logical coordinates and clipped to the output.
        CaptureOutputRegion = 1,
        /// Unbind the factory (frames already created stay valid).
        Destroy = 2,
    }
}

requests! {
    /// `zwlr_screencopy_frame_v1` requests.
    ScreencopyFrameRequest {
        /// Copy this frame into `buffer`, which must match the size/stride/
        /// format from the `buffer` event. Answered by `flags` + `ready`, or
        /// by `failed`.
        Copy = 0,
        /// Delete this frame object, used or not. Required after both
        /// `ready` and `failed`: a frame is single-use.
        Destroy = 1,
        /// Like `Copy`, but defers until the capture source is damaged (v2+).
        CopyWithDamage = 2,
    }
}

events! {
    /// `zwlr_screencopy_frame_v1` events.
    ScreencopyFrameEvent {
        /// The shm buffer parameters to allocate: `format` (a
        /// [`Format`](super::Format) code), `width`, `height`, `stride`.
        Buffer = 0,
        /// Frame properties, sent once before `ready` — see
        /// [`ScreencopyFrameFlags`].
        Flags = 1,
        /// The copy completed and the buffer is readable: `tv_sec_hi`,
        /// `tv_sec_lo`, `tv_nsec` presentation timestamp.
        Ready = 2,
        /// The copy failed; nothing was written. Destroy the frame.
        Failed = 3,
        /// Damage box accompanying a `copy_with_damage` (v2+): `x`, `y`,
        /// `width`, `height`.
        Damage = 4,
        /// dmabuf buffer parameters (v3+): `format` (a DRM fourcc), `width`,
        /// `height`. Ignored — we capture into shm.
        LinuxDmabuf = 5,
        /// End of the buffer-parameter events (v3+). Versions below 3 never
        /// send it, so `buffer` alone must also be treated as sufficient.
        BufferDone = 6,
    }
}

/// `zwlr_screencopy_frame_v1` frame flags (the `flags` event), `ORed` together.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ScreencopyFrameFlags {
    /// The copied contents are bottom-up: row 0 of the buffer is the last
    /// row of the image, so the capture must be flipped vertically.
    YInvert = 1,
}

impl ScreencopyFrameFlags {
    /// Whether this flag is set in a `flags` event bitmask.
    #[must_use]
    pub const fn in_mask(self, mask: u32) -> bool {
        mask & (self as u32) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn screencopy_frame_event_opcodes_match_the_protocol_xml() {
        use ScreencopyFrameEvent as E;
        for (op, want) in [
            (0, E::Buffer),
            (1, E::Flags),
            (2, E::Ready),
            (3, E::Failed),
            (4, E::Damage),
            (5, E::LinuxDmabuf),
            (6, E::BufferDone),
        ] {
            assert_eq!(E::from_opcode(op), Some(want));
        }
        assert_eq!(E::from_opcode(7), None);
    }

    #[test]
    fn screencopy_request_opcodes_match_the_protocol_xml() {
        assert_eq!(ScreencopyManagerRequest::CaptureOutput.opcode(), 0);
        assert_eq!(ScreencopyManagerRequest::CaptureOutputRegion.opcode(), 1);
        assert_eq!(ScreencopyManagerRequest::Destroy.opcode(), 2);
        assert_eq!(ScreencopyFrameRequest::Copy.opcode(), 0);
        assert_eq!(ScreencopyFrameRequest::Destroy.opcode(), 1);
        assert_eq!(ScreencopyFrameRequest::CopyWithDamage.opcode(), 2);
    }

    #[test]
    fn screencopy_y_invert_flag_reads_its_bit() {
        assert!(ScreencopyFrameFlags::YInvert.in_mask(1));
        assert!(!ScreencopyFrameFlags::YInvert.in_mask(0));
        assert!(ScreencopyFrameFlags::YInvert.in_mask(0b1011));
        assert!(!ScreencopyFrameFlags::YInvert.in_mask(0b1010));
    }
}
