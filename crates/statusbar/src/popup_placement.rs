//! Where a panel popup lands relative to the bar.
//!
//! Placement is a pure function of the clicked module's x-span, the
//! panel's width, the bar's logical width, and the `[panel]` gap: pick a
//! side (configured, or auto-flipped to whichever fits), clamp to the
//! screen edges with a gap, and drop below (or above) the bar by the same
//! gap. The result is an `xdg_positioner` description relative to the bar
//! surface.

use config::{BarPosition, PanelSide};
use platform::popup::PopupPositioner;
use platform::protocol::{ConstraintAdjustment, PositionerAnchor, PositionerGravity};

/// The clicked bar module's horizontal span plus the measurements
/// placement needs, all in the bar's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleAnchor {
    pub x0: i32,
    pub x1: i32,
    pub bar_height: i32,
    /// The screen edge the bar docks to; the panel opens away from it.
    pub position: BarPosition,
}

/// A resolved popup position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchored {
    /// The panel's left edge, bar-relative.
    pub left: i32,
    pub gap: i32,
    pub bar_height: i32,
    pub position: BarPosition,
}

impl Anchored {
    /// Resolve where a `panel_w`-wide popup opens beside the module
    /// described by `anchor`, on a bar `screen_w` logical px wide.
    ///
    /// * `Left` -- the panel's left edge follows the module's (`x0`).
    /// * `Right` -- the panel's right edge follows the module's (`x1`).
    /// * `Auto` -- `Left` when the panel fits to the right of `x0`,
    ///   `Right` otherwise.
    ///
    /// The result is clamped to `[gap, screen_w - gap - panel_w]`.
    #[must_use]
    pub fn resolve(
        anchor: ModuleAnchor,
        side: PanelSide,
        panel_w: i32,
        screen_w: i32,
        gap: i32,
    ) -> Self {
        let left = match side {
            PanelSide::Left => anchor.x0,
            PanelSide::Right => anchor.x1 - panel_w,
            PanelSide::Auto => {
                if anchor.x0 + gap + panel_w <= screen_w - gap {
                    anchor.x0
                } else {
                    anchor.x1 - panel_w
                }
            }
        };
        let max_left = screen_w - gap - panel_w;
        let clamped = if max_left < gap {
            gap
        } else {
            left.clamp(gap, max_left)
        };
        Self {
            left: clamped,
            gap,
            bar_height: anchor.bar_height,
            position: anchor.position,
        }
    }

    /// The positioner placing a `size` popup here, relative to the bar.
    #[must_use]
    pub fn positioner(&self, size: (i32, i32)) -> PopupPositioner {
        let (anchor, gravity, dy) = match self.position {
            BarPosition::Top => (
                PositionerAnchor::BottomLeft,
                PositionerGravity::BottomRight,
                self.gap,
            ),
            BarPosition::Bottom => (
                PositionerAnchor::TopLeft,
                PositionerGravity::TopRight,
                -self.gap,
            ),
        };
        PopupPositioner {
            size,
            anchor_rect: (self.left, 0, 1, self.bar_height.max(1)),
            anchor,
            gravity,
            offset: (0, dy),
            constraint_adjustment: ConstraintAdjustment::bits(&[
                ConstraintAdjustment::SlideX,
                ConstraintAdjustment::SlideY,
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANCHOR: ModuleAnchor = ModuleAnchor {
        x0: 600,
        x1: 680,
        bar_height: 28,
        position: BarPosition::Top,
    };

    #[test]
    fn left_side_opens_right_of_the_module() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Left, 400, 1920, 8);
        assert_eq!(a.left, 600);
    }

    #[test]
    fn right_side_opens_left_of_the_module() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Right, 400, 1920, 8);
        assert_eq!(a.left, 680 - 400);
    }

    #[test]
    fn auto_opens_right_mid_bar() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Auto, 400, 1920, 8);
        assert_eq!(a.left, 600);
    }

    #[test]
    fn auto_flips_near_the_right_edge() {
        let anchor = ModuleAnchor {
            x0: 1800,
            x1: 1880,
            ..ANCHOR
        };
        let a = Anchored::resolve(anchor, PanelSide::Auto, 400, 1920, 8);
        assert_eq!(a.left, 1880 - 400);
    }

    #[test]
    fn clamping_never_touches_an_edge() {
        let anchor = ModuleAnchor {
            x0: 1900,
            x1: 1915,
            ..ANCHOR
        };
        let a = Anchored::resolve(anchor, PanelSide::Left, 400, 1920, 8);
        assert_eq!(a.left, 1920 - 8 - 400);
        let anchor = ModuleAnchor {
            x0: 5,
            x1: 40,
            ..ANCHOR
        };
        let a = Anchored::resolve(anchor, PanelSide::Right, 400, 1920, 8);
        assert_eq!(a.left, 8);
    }

    #[test]
    fn a_narrower_output_than_the_panel_hugs_the_left_gap() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Auto, 500, 400, 8);
        assert_eq!(a.left, 8);
    }

    #[test]
    fn a_top_bar_drops_the_popup_below_it_by_the_gap() {
        let p = Anchored::resolve(ANCHOR, PanelSide::Left, 400, 1920, 8).positioner((400, 300));
        assert_eq!(p.size, (400, 300));
        assert_eq!(p.anchor_rect, (600, 0, 1, 28));
        assert_eq!(p.anchor, PositionerAnchor::BottomLeft);
        assert_eq!(p.gravity, PositionerGravity::BottomRight);
        assert_eq!(p.offset, (0, 8));
    }

    #[test]
    fn a_bottom_bar_raises_the_popup_above_it_by_the_gap() {
        let anchor = ModuleAnchor {
            position: BarPosition::Bottom,
            ..ANCHOR
        };
        let p = Anchored::resolve(anchor, PanelSide::Left, 400, 1920, 8).positioner((400, 300));
        assert_eq!(p.anchor, PositionerAnchor::TopLeft);
        assert_eq!(p.gravity, PositionerGravity::TopRight);
        assert_eq!(p.offset, (0, -8));
    }

    #[test]
    fn zero_gap_sits_flush_against_the_bar() {
        let p = Anchored::resolve(ANCHOR, PanelSide::Left, 400, 1920, 0).positioner((400, 300));
        assert_eq!(p.offset, (0, 0));
    }
}
