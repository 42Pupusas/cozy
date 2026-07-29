//! [`DragArm`] -- deciding when a press-and-move becomes a drag.
//!
//! A single click must not start a drag: a mouse's inherent jitter between
//! button-down and button-up would otherwise turn every ordinary click into
//! an accidental drag-and-drop. The fix is the same one every GUI toolkit
//! uses -- arm on press, and only declare a drag once the pointer has moved
//! past a small pixel threshold -- but the *decision* is pure arithmetic over
//! two points, with no knowledge of what is being dragged or where it might
//! land. Keeping it here, rather than inline in a window's pointer handler,
//! is what makes the threshold itself unit-testable without a compositor.
//!
//! This type answers only "has a drag started"; it carries no payload and no
//! target. What is being dragged and where it can land are the caller's
//! business -- this just knows the geometry rule for "did the user mean to
//! drag".

/// Tracks one potential drag from its originating press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DragArm {
    /// The press position, while the pointer hasn't yet moved far enough to
    /// count as a drag. `None` once consumed (by a fired threshold or an
    /// explicit cancel) or before any press.
    origin: Option<(i32, i32)>,
}

impl DragArm {
    /// Pixels the pointer must move from its press position before that
    /// press counts as a drag rather than an unsteady click.
    pub const THRESHOLD: i32 = 4;

    /// No press armed yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { origin: None }
    }

    /// Arm a potential drag at the press position `at`.
    pub const fn press(&mut self, at: (i32, i32)) {
        self.origin = Some(at);
    }

    /// Report motion to `at`. Returns `true` exactly once per armed press --
    /// the moment the pointer first moves past [`Self::THRESHOLD`] from the
    /// press position -- and disarms itself so a caller cannot start a
    /// second drag from the same press.
    ///
    /// Returns `false` with nothing armed (no press yet, or the threshold
    /// already fired for this press), so a caller can call this
    /// unconditionally on every motion event.
    pub const fn motion_exceeds_threshold(&mut self, at: (i32, i32)) -> bool {
        let Some((ox, oy)) = self.origin else {
            return false;
        };
        let (dx, dy) = (at.0 - ox, at.1 - oy);
        if dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
            >= Self::THRESHOLD.saturating_mul(Self::THRESHOLD)
        {
            self.origin = None;
            true
        } else {
            false
        }
    }

    /// Disarm without firing -- a release before the threshold was reached,
    /// or the pointer leaving the surface entirely.
    pub const fn cancel(&mut self) {
        self.origin = None;
    }

    /// Whether a press is currently armed (not yet fired, not cancelled).
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.origin.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::DragArm;

    #[test]
    fn a_fresh_arm_has_nothing_armed() {
        let arm = DragArm::new();
        assert!(!arm.is_armed());
    }

    #[test]
    fn pressing_arms_it() {
        let mut arm = DragArm::new();
        arm.press((10, 10));
        assert!(arm.is_armed());
    }

    #[test]
    fn tiny_motion_does_not_fire() {
        let mut arm = DragArm::new();
        arm.press((10, 10));
        assert!(!arm.motion_exceeds_threshold((11, 10)));
        assert!(arm.is_armed(), "must stay armed under threshold");
    }

    #[test]
    fn motion_past_the_threshold_fires_exactly_once() {
        let mut arm = DragArm::new();
        arm.press((0, 0));
        assert!(arm.motion_exceeds_threshold((10, 0)));
        assert!(!arm.is_armed(), "firing must disarm");
        // A second motion, even further away, must not fire again for the
        // same press -- the caller already started the drag.
        assert!(!arm.motion_exceeds_threshold((20, 0)));
    }

    #[test]
    fn diagonal_motion_uses_euclidean_distance_not_either_axis_alone() {
        // 3-4-5 triangle: neither axis alone crosses a threshold of 4, but
        // the true distance (5) does.
        let mut arm = DragArm::new();
        arm.press((0, 0));
        assert!(arm.motion_exceeds_threshold((3, 4)));
    }

    #[test]
    fn motion_with_nothing_armed_never_fires() {
        let mut arm = DragArm::new();
        assert!(!arm.motion_exceeds_threshold((1000, 1000)));
    }

    #[test]
    fn cancel_disarms_without_firing() {
        let mut arm = DragArm::new();
        arm.press((0, 0));
        arm.cancel();
        assert!(!arm.is_armed());
        assert!(!arm.motion_exceeds_threshold((100, 100)));
    }

    #[test]
    fn a_new_press_replaces_an_earlier_unfired_one() {
        let mut arm = DragArm::new();
        arm.press((0, 0));
        arm.press((50, 50));
        // Distance from the *new* origin, not the old one.
        assert!(!arm.motion_exceeds_threshold((52, 50)));
        assert!(arm.motion_exceeds_threshold((60, 50)));
    }

    #[test]
    fn negative_coordinates_are_handled() {
        let mut arm = DragArm::new();
        arm.press((-5, -5));
        assert!(arm.motion_exceeds_threshold((-15, -5)));
    }
}
