//! [`LogicalRect`] -- a section's placement in surface-local logical pixels,
//! the space `wl_subsurface.set_position` and `wp_viewport.set_destination`
//! speak.

use crate::draw::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogicalRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl LogicalRect {
    /// Convert a physical rect by dividing its edges, not its size, so
    /// neighbouring rects that share an edge still share it after scaling.
    #[must_use]
    pub fn from_physical(rect: Rect, scale: i32) -> Self {
        let scale = scale.max(1);
        let x0 = rect.x.div_euclid(scale);
        let y0 = rect.y.div_euclid(scale);
        let x1 = rect.right().div_euclid(scale);
        let y1 = rect.bottom().div_euclid(scale);
        Self {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }

    /// `wp_viewport.set_destination` rejects a zero or negative size.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.w > 0 && self.h > 0
    }
}

#[cfg(test)]
mod tests {
    use super::LogicalRect;
    use crate::draw::Rect;

    #[test]
    fn scale_one_is_the_identity() {
        let r = LogicalRect::from_physical(Rect::new(3, 4, 50, 60), 1);
        assert_eq!(
            r,
            LogicalRect {
                x: 3,
                y: 4,
                w: 50,
                h: 60
            }
        );
    }

    #[test]
    fn shared_edges_stay_shared_at_scale_two() {
        let left = LogicalRect::from_physical(Rect::new(0, 0, 101, 10), 2);
        let right = LogicalRect::from_physical(Rect::new(101, 0, 99, 10), 2);
        assert_eq!(left.x + left.w, right.x);
        assert_eq!(right.x + right.w, 100);
    }

    #[test]
    fn a_sliver_thinner_than_one_logical_pixel_is_hidden() {
        let r = LogicalRect::from_physical(Rect::new(10, 10, 1, 40), 2);
        assert!(!r.is_visible());
    }

    #[test]
    fn a_zero_scale_is_treated_as_one() {
        let r = LogicalRect::from_physical(Rect::new(0, 0, 8, 8), 0);
        assert_eq!(r.w, 8);
    }
}
