//! [`Camera`] -- the infinite canvas's viewport: pan offset and zoom level.
//!
//! The canvas itself is an unbounded 2D plane; the camera is the finite
//! window onto it. Every interaction (pan, zoom-at-cursor, scroll) and
//! every paint goes through the one pair of transforms here, so there is
//! exactly one place where world and screen coordinates can disagree.

/// How far the camera can zoom out before the canvas stops being readable.
const MIN_ZOOM: f64 = 0.1;
/// How far the camera can zoom in before individual pixels of a glyph
/// dominate the screen.
const MAX_ZOOM: f64 = 8.0;

/// The discrete zoom steps the keyboard cycles through. Geometric, so each
/// step feels like the same size of jump.
pub const ZOOM_STEPS: [f64; 7] = [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0];

/// A pan offset plus zoom over the infinite plane.
///
/// World coordinates are in "world units" (arbitrary but stable); screen
/// coordinates are physical pixels. The camera maps world -> screen as
/// `screen = (world * zoom) + offset`, i.e. `offset` is *which world point
/// sits at the screen origin* already scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    offset: (f64, f64),
    zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            offset: (0.0, 0.0),
            zoom: 1.0,
        }
    }
}

impl Camera {
    /// The current zoom factor.
    #[must_use]
    pub const fn zoom(&self) -> f64 {
        self.zoom
    }

    /// The current pan offset, in scaled world units.
    #[must_use]
    pub const fn offset(&self) -> (f64, f64) {
        self.offset
    }

    /// Restore a persisted camera. `zoom` is clamped into the legal range;
    /// a stored out-of-range value (from a future version, say) must not
    /// break the mapping.
    #[must_use]
    pub const fn restored(offset: (f64, f64), zoom: f64) -> Self {
        Self {
            offset,
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }

    /// World -> screen.
    #[must_use]
    #[allow(clippy::wrong_self_convention)]
    pub fn to_screen(self, world: (f64, f64)) -> (i32, i32) {
        (
            Self::round_to_i32(world.0.mul_add(self.zoom, self.offset.0)),
            Self::round_to_i32(world.1.mul_add(self.zoom, self.offset.1)),
        )
    }

    /// Screen -> world, for hit-testing and for anchoring zooms.
    #[must_use]
    #[allow(clippy::wrong_self_convention)]
    pub fn to_world(self, screen: (i32, i32)) -> (f64, f64) {
        let (sx, sy) = (f64::from(screen.0), f64::from(screen.1));
        (
            (sx - self.offset.0) / self.zoom,
            (sy - self.offset.1) / self.zoom,
        )
    }

    /// Pan by a screen-space delta (already physical pixels).
    pub fn pan_by(&mut self, dx: i32, dy: i32) {
        self.offset.0 += f64::from(dx);
        self.offset.1 += f64::from(dy);
    }

    /// Zoom by `factor`, keeping the world point under `anchor` (a screen
    /// position) fixed on screen.
    ///
    /// Returns whether the zoom actually changed, so a caller at the clamp
    /// bound can skip the repaint.
    pub fn zoom_at(&mut self, anchor: (i32, i32), factor: f64) -> bool {
        let before = self.zoom;
        let world = self.to_world(anchor);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (self.zoom - before).abs() < f64::EPSILON {
            return false;
        }
        self.offset.0 = world.0.mul_add(-self.zoom, f64::from(anchor.0));
        self.offset.1 = world.1.mul_add(-self.zoom, f64::from(anchor.1));
        true
    }

    /// Step to the next (or previous) discrete zoom step, keeping `anchor`
    /// fixed. Returns whether the zoom changed.
    ///
    /// The ladder clamps at its own ends, not at [`MIN_ZOOM`]/[`MAX_ZOOM`]:
    /// a step up from the top rung is a no-op rather than a jump to the
    /// wheel-only extreme.
    pub fn zoom_step(&mut self, anchor: (i32, i32), up: bool) -> bool {
        let current = self.zoom;
        let next = if up {
            ZOOM_STEPS.iter().find(|z| **z > current + f64::EPSILON)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .find(|z| **z < current - f64::EPSILON)
        };
        let Some(next) = next.copied() else {
            return false;
        };
        self.zoom_at(anchor, next / current)
    }

    /// Reset to identity: centered origin at 100%.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Round an `f64` pixel coordinate to `i32` saturating, the one
    /// conversion this module needs (world coordinates are always
    /// window-scale magnitudes).
    #[allow(clippy::cast_possible_truncation)]
    fn round_to_i32(value: f64) -> i32 {
        if value.is_nan() {
            return 0;
        }
        i32::try_from(value.round() as i64).unwrap_or(i32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_camera_maps_world_straight_to_screen() {
        let camera = Camera::default();
        assert_eq!(camera.to_screen((100.0, 50.0)), (100, 50));
        let world = camera.to_world((100, 50));
        assert!((world.0 - 100.0).abs() < 1e-9);
        assert!((world.1 - 50.0).abs() < 1e-9);
    }

    #[test]
    fn to_world_inverts_to_screen_within_one_pixel() {
        // `to_screen` lands on whole pixels, so the round trip can only be
        // exact to within half a pixel expressed in world units.
        let camera = Camera {
            offset: (33.0, -12.0),
            zoom: 1.75,
        };
        let tolerance = 0.5 / camera.zoom() + 1e-9;
        for world in [(0.0, 0.0), (-500.0, 250.0), (1234.5, -99.25)] {
            let screen = camera.to_screen(world);
            let back = camera.to_world(screen);
            assert!(
                (back.0 - world.0).abs() <= tolerance,
                "{back:?} vs {world:?}"
            );
            assert!((back.1 - world.1).abs() <= tolerance);
        }
    }

    #[test]
    fn panning_moves_the_screen_origin() {
        let mut camera = Camera::default();
        camera.pan_by(40, -20);
        assert_eq!(camera.to_screen((0.0, 0.0)), (40, -20));
    }

    #[test]
    fn zooming_in_grows_a_world_unit() {
        let mut camera = Camera::default();
        assert!(camera.zoom_at((0, 0), 2.0));
        assert_eq!(camera.to_screen((10.0, 0.0)).0, 20);
    }

    #[test]
    fn zooming_keeps_the_anchor_world_point_fixed() {
        let mut camera = Camera::default();
        camera.pan_by(60, 30);
        let anchor_screen = (200, 150);
        let world_before = camera.to_world(anchor_screen);
        assert!(camera.zoom_at(anchor_screen, 1.3));
        let world_after = camera.to_world(anchor_screen);
        assert!(
            (world_after.0 - world_before.0).abs() < 1e-6,
            "anchor drifted: {world_before:?} -> {world_after:?}"
        );
    }

    #[test]
    fn zoom_clamps_at_both_ends() {
        let mut camera = Camera::default();
        for _ in 0..40 {
            camera.zoom_at((0, 0), 3.0);
        }
        assert!((camera.zoom() - 8.0).abs() < 1e-9);
        for _ in 0..40 {
            camera.zoom_at((0, 0), 1.0 / 3.0);
        }
        assert!((camera.zoom() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn a_clamped_zoom_reports_no_change() {
        let mut camera = Camera::default();
        for _ in 0..40 {
            camera.zoom_at((0, 0), 3.0);
        }
        assert!(
            !camera.zoom_at((0, 0), 1.5),
            "at the clamp, nothing changes"
        );
    }

    #[test]
    fn zoom_steps_walk_the_ladder_in_both_directions() {
        let mut camera = Camera::default();
        assert!(camera.zoom_step((0, 0), true));
        assert!((camera.zoom() - 1.5).abs() < 1e-9);
        // Two more steps: 2.0 then 3.0.
        assert!(camera.zoom_step((0, 0), true));
        assert!((camera.zoom() - 2.0).abs() < 1e-9);
        assert!(camera.zoom_step((0, 0), true));
        assert!((camera.zoom() - 3.0).abs() < 1e-9);
        // At the top, another step up is a no-op.
        assert!(!camera.zoom_step((0, 0), true));
        // Back down lands on 2.0, not 3.0 again.
        assert!(camera.zoom_step((0, 0), false));
        assert!((camera.zoom() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_zoom_between_steps_snaps_to_the_nearest_ladder_rung() {
        // zoomed to 1.7 by wheel, a step-up must pick 2.0 (the next rung
        // strictly above), not 3.0.
        let mut camera = Camera::default();
        camera.zoom_at((0, 0), 1.7);
        assert!(camera.zoom_step((0, 0), true));
        assert!((camera.zoom() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_restored_camera_clamps_an_out_of_range_zoom() {
        let camera = Camera::restored((10.0, 10.0), 100.0);
        assert!((camera.zoom() - 8.0).abs() < 1e-9);
        let camera = Camera::restored((10.0, 10.0), 0.0);
        assert!((camera.zoom() - 0.1).abs() < 1e-9);
    }

    #[test]
    fn reset_returns_to_the_identity_mapping() {
        let mut camera = Camera {
            offset: (500.0, -250.0),
            zoom: 2.5,
        };
        camera.reset();
        assert_eq!(camera, Camera::default());
    }
}
