//! Mapping a selection from surface coordinates onto the captured buffer.

use screencopy::CaptureRegion;

/// The ratio between the overlay's logical size and the frozen buffer's
/// pixel size, which differ on any scaled output.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RegionScale {
    surface: (i32, i32),
    buffer: (u32, u32),
}

impl RegionScale {
    #[must_use]
    pub const fn new(surface: (i32, i32), buffer: (u32, u32)) -> Self {
        Self { surface, buffer }
    }

    /// `region` in buffer pixels, widened outward so a selected logical pixel
    /// is never partially dropped, and clamped to the buffer.
    #[must_use]
    pub fn to_buffer(self, region: CaptureRegion) -> CaptureRegion {
        let (x0, x1) = Self::span(region.x, region.width, self.surface.0, self.buffer.0);
        let (y0, y1) = Self::span(region.y, region.height, self.surface.1, self.buffer.1);
        CaptureRegion {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    fn span(start: i32, length: i32, logical: i32, pixels: u32) -> (i32, i32) {
        let logical = i64::from(logical.max(1));
        let pixels = i64::from(pixels);
        let first = (i64::from(start) * pixels).div_euclid(logical);
        let last = (i64::from(start) + i64::from(length)) * pixels;
        let last = last.div_euclid(logical) + i64::from(last.rem_euclid(logical) != 0);
        let clamp = |v: i64| i32::try_from(v.clamp(0, pixels)).unwrap_or(i32::MAX);
        (clamp(first), clamp(last))
    }
}

#[cfg(test)]
mod tests {
    use super::RegionScale;
    use screencopy::CaptureRegion;

    struct Fixture;

    impl Fixture {
        const fn region(x: i32, y: i32, width: i32, height: i32) -> CaptureRegion {
            CaptureRegion {
                x,
                y,
                width,
                height,
            }
        }
    }

    #[test]
    fn an_unscaled_output_maps_one_to_one() {
        let scale = RegionScale::new((1920, 1080), (1920, 1080));
        let region = Fixture::region(10, 20, 300, 400);
        assert_eq!(scale.to_buffer(region), region);
    }

    #[test]
    fn a_double_scale_output_doubles_every_edge() {
        let scale = RegionScale::new((1920, 1080), (3840, 2160));
        assert_eq!(
            scale.to_buffer(Fixture::region(10, 20, 300, 400)),
            Fixture::region(20, 40, 600, 800)
        );
    }

    #[test]
    fn a_fractional_scale_widens_outward_rather_than_cutting_pixels() {
        let scale = RegionScale::new((1536, 864), (1920, 1080));
        assert_eq!(
            scale.to_buffer(Fixture::region(1, 1, 1, 1)),
            Fixture::region(1, 1, 2, 2)
        );
    }

    #[test]
    fn the_result_never_leaves_the_buffer() {
        let scale = RegionScale::new((100, 100), (150, 150));
        assert_eq!(
            scale.to_buffer(Fixture::region(90, 90, 10, 10)),
            Fixture::region(135, 135, 15, 15)
        );
    }
}
