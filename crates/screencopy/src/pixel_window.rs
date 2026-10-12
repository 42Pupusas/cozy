//! A validated rectangle of a captured buffer.

use crate::capture_region::CaptureRegion;

/// A rectangle proven to lie inside a `width` x `height` area, in unsigned
/// buffer pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PixelWindow {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelWindow {
    /// The whole `width` x `height` area.
    #[must_use]
    pub const fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// `region`, relative to this window, as a window in the same frame as
    /// `self`; `None` when it is empty or leaves this window.
    #[must_use]
    pub fn sub(&self, region: CaptureRegion) -> Option<Self> {
        let x = u32::try_from(region.x).ok()?;
        let y = u32::try_from(region.y).ok()?;
        let width = u32::try_from(region.width).ok().filter(|w| *w > 0)?;
        let height = u32::try_from(region.height).ok().filter(|h| *h > 0)?;
        let fits = x.checked_add(width)? <= self.width && y.checked_add(height)? <= self.height;
        fits.then_some(Self {
            x: self.x + x,
            y: self.y + y,
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::PixelWindow;
    use crate::capture_region::CaptureRegion;

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
    fn a_region_touching_the_far_edges_is_accepted() {
        assert_eq!(
            PixelWindow::whole(10, 6).sub(Fixture::region(4, 2, 6, 4)),
            Some(PixelWindow {
                x: 4,
                y: 2,
                width: 6,
                height: 4
            })
        );
    }

    #[test]
    fn regions_leaving_the_window_or_without_area_are_refused() {
        let whole = PixelWindow::whole(10, 6);
        for region in [
            Fixture::region(5, 0, 6, 1),
            Fixture::region(0, 3, 1, 4),
            Fixture::region(-1, 0, 2, 2),
            Fixture::region(0, -1, 2, 2),
            Fixture::region(0, 0, 0, 2),
            Fixture::region(0, 0, 2, 0),
            Fixture::region(i32::MAX, 0, i32::MAX, 1),
        ] {
            assert_eq!(whole.sub(region), None, "{region:?}");
        }
    }

    #[test]
    fn a_nested_window_is_offset_by_its_parent_origin() {
        let outer = PixelWindow {
            x: 3,
            y: 4,
            width: 5,
            height: 5,
        };
        assert_eq!(
            outer.sub(Fixture::region(1, 2, 2, 1)),
            Some(PixelWindow {
                x: 4,
                y: 6,
                width: 2,
                height: 1
            })
        );
        assert_eq!(outer.sub(Fixture::region(4, 0, 2, 1)), None);
    }
}
