//! `wp_single_pixel_buffer_manager_v1`: a `wl_buffer` holding one colour,
//! with no client-side memory behind it.

use super::Interface;

pub const WP_SINGLE_PIXEL_BUFFER_MANAGER_V1: Interface = Interface {
    name: "wp_single_pixel_buffer_manager_v1",
    version: 1,
};

requests! {
    /// `wp_single_pixel_buffer_manager_v1` requests.
    SinglePixelBufferManagerRequest {
        Destroy = 0,
        /// `id` (`new_id` `wl_buffer`), then premultiplied `r`, `g`, `b`, `a`,
        /// each a full-range `u32`.
        CreateU32RgbaBuffer = 1,
    }
}

/// One premultiplied colour channel widened to the protocol's full `u32`
/// range, where `u32::MAX` is full intensity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct U32Channel(pub u32);

impl U32Channel {
    /// Widen an 8-bit channel so `0xff` maps exactly to `u32::MAX`.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        Self(value as u32 * 0x0101_0101)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn single_pixel_buffer_opcodes_match_the_protocol_xml() {
        assert_eq!(SinglePixelBufferManagerRequest::Destroy.opcode(), 0);
        assert_eq!(
            SinglePixelBufferManagerRequest::CreateU32RgbaBuffer.opcode(),
            1
        );
    }

    #[test]
    fn u8_channel_widens_to_the_full_u32_range() {
        assert_eq!(U32Channel::from_u8(0), U32Channel(0));
        assert_eq!(U32Channel::from_u8(0xff), U32Channel(u32::MAX));
        assert_eq!(U32Channel::from_u8(0x80), U32Channel(0x8080_8080));
    }
}
