//! The rectangle a capture is restricted to.

/// A rectangle in an output's logical coordinates when sent to the
/// compositor, or in buffer pixels when cropping a frame already captured.
///
/// Kept distinct from a bare tuple because `capture_output_region` takes the
/// four numbers positionally, and transposing x/y or width/height there yields
/// a plausible-looking screenshot of the wrong place.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CaptureRegion {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
