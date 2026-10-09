//! `wp_viewporter`: per-surface crop and scale. `set_destination` gives a
//! surface a logical size independent of its buffer, which is what lets a
//! 1×1 buffer cover a rectangle of any size.

use super::Interface;

pub const WP_VIEWPORTER: Interface = Interface {
    name: "wp_viewporter",
    version: 1,
};

requests! {
    /// `wp_viewporter` requests.
    ViewporterRequest {
        Destroy = 0,
        /// Create a viewport for `surface`: `id` (`new_id`), `surface`.
        GetViewport = 1,
    }
}

requests! {
    /// `wp_viewport` requests.
    ViewportRequest {
        Destroy = 0,
        /// Crop the buffer: `x`, `y`, `width`, `height` as `wl_fixed`;
        /// all `-1` unsets the crop.
        SetSource = 1,
        /// Surface size in logical pixels: `width`, `height`; both `-1`
        /// unsets it.
        SetDestination = 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Opcode;

    #[test]
    fn viewporter_opcodes_match_the_protocol_xml() {
        assert_eq!(ViewporterRequest::Destroy.opcode(), 0);
        assert_eq!(ViewporterRequest::GetViewport.opcode(), 1);
        assert_eq!(ViewportRequest::Destroy.opcode(), 0);
        assert_eq!(ViewportRequest::SetSource.opcode(), 1);
        assert_eq!(ViewportRequest::SetDestination.opcode(), 2);
    }
}
