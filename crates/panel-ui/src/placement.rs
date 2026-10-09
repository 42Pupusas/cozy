//! Where a popup panel lands on screen.
//!
//! Placement is a pure function of the clicked module's x-span, the
//! panel's width, the output's logical width, and the `[panel]` gap:
//! pick a side (configured, or auto-flipped to whichever fits), clamp
//! to the screen edges with a gap, and drop below the bar with the
//! same gap. Kept free of any Wayland types so every rule below is
//! unit-testable without a compositor.

use config::{BarPosition, PanelSide};

/// The clicked bar module's horizontal span plus the measurements
/// placement needs. `x1` defaults to `x0` for callers that only know
/// the module's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleAnchor {
    /// The module's left edge, bar-relative logical px.
    pub x0: i32,
    /// The module's right edge, bar-relative logical px.
    pub x1: i32,
    /// The bar's thickness, logical px.
    pub bar_height: i32,
    /// The screen edge the bar docks to; the panel opens away from it.
    pub position: BarPosition,
}

/// A resolved popup position, as a layer-surface margin pair.
///
/// The surface is anchored `Left` plus `Top` or `Bottom` to match the bar,
/// so the left margin and the margin from the bar's edge fully determine
/// where it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchored {
    pub left: i32,
    /// Distance from the screen edge the bar sits on, past the bar and gap.
    pub vertical: i32,
    pub position: BarPosition,
    /// Which way the panel ended up opening -- reported so a test (or a
    /// `--side auto` user) can see the flip decision.
    pub side: PanelSide,
}

impl Anchored {
    /// Resolve where a `panel_w`-wide popup opens below the module
    /// described by `anchor`, on an output `screen_w` logical px wide,
    /// with `gap` px of floating distance to every edge (the bar's
    /// bottom edge and both screen edges).
    ///
    /// * `Left` -- the panel's left edge follows the module's (`x0`),
    ///   opening to the right of it.
    /// * `Right` -- the panel's right edge follows the module's (`x1`),
    ///   opening to the left of it.
    /// * `Auto` -- `Left` when the panel fits on-screen to the right of
    ///   `x0`, `Right` otherwise (a module near the right edge of the
    ///   bar flips, one mid-bar does not).
    ///
    /// Whatever side wins, the result is clamped to
    /// `[gap, screen_w - gap - panel_w]` so the panel never touches an
    /// edge (or leaves the screen) even when the module sits at the
    /// far end of a bar narrower than the panel.
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
            // Output narrower than panel + two gaps: hug the left edge
            // rather than parking (possibly negatively) off-screen.
            gap
        } else {
            left.clamp(gap, max_left)
        };
        Self {
            left: clamped,
            vertical: anchor.bar_height + gap,
            position: anchor.position,
            side,
        }
    }
}

impl Anchored {
    /// Layer-surface margin as `(top, right, bottom, left)`.
    #[must_use]
    pub const fn margin(&self) -> (i32, i32, i32, i32) {
        match self.position {
            BarPosition::Top => (self.vertical, 0, 0, self.left),
            BarPosition::Bottom => (0, 0, self.vertical, self.left),
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
        assert_eq!(a.vertical, 28 + 8, "the gap floats the panel off the bar");
        assert_eq!(a.position, BarPosition::Top);
    }

    #[test]
    fn bottom_bar_measures_the_margin_from_the_bottom_edge() {
        let anchor = ModuleAnchor {
            position: BarPosition::Bottom,
            ..ANCHOR
        };
        let a = Anchored::resolve(anchor, PanelSide::Left, 400, 1920, 8);
        assert_eq!(a.vertical, 28 + 8, "clear of the bar, same gap");
        assert_eq!(a.position, BarPosition::Bottom);
        assert_eq!(a.left, 600, "horizontal placement is unaffected");
        assert_eq!(a.margin(), (0, 0, 36, 600));
    }

    #[test]
    fn top_bar_margin_is_measured_from_the_top() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Left, 400, 1920, 8);
        assert_eq!(a.margin(), (36, 0, 0, 600));
    }

    #[test]
    fn right_side_opens_left_of_the_module() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Right, 400, 1920, 8);
        assert_eq!(a.left, 680 - 400, "right edge follows the module's");
    }

    #[test]
    fn auto_opens_right_mid_bar() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Auto, 400, 1920, 8);
        assert_eq!(a.left, 600, "plenty of room to the right");
    }

    #[test]
    fn auto_flips_near_the_right_edge() {
        let anchor = ModuleAnchor {
            x0: 1800,
            x1: 1880,
            bar_height: 28,
            position: BarPosition::Top,
        };
        let a = Anchored::resolve(anchor, PanelSide::Auto, 400, 1920, 8);
        assert_eq!(
            a.left,
            1880 - 400,
            "no room rightward: open leftward from the module's right edge"
        );
    }

    #[test]
    fn clamping_never_touches_an_edge() {
        // Module at the very right edge asking to open rightward.
        let anchor = ModuleAnchor {
            x0: 1900,
            x1: 1915,
            bar_height: 28,
            position: BarPosition::Top,
        };
        let a = Anchored::resolve(anchor, PanelSide::Left, 400, 1920, 8);
        assert_eq!(a.left, 1920 - 8 - 400, "clamped to gap-from-right-edge");
        // Module at the very left edge asking to open leftward.
        let anchor = ModuleAnchor {
            x0: 5,
            x1: 40,
            bar_height: 28,
            position: BarPosition::Top,
        };
        let a = Anchored::resolve(anchor, PanelSide::Right, 400, 1920, 8);
        assert_eq!(a.left, 8, "clamped to gap-from-left-edge");
    }

    #[test]
    fn a_narrower_output_than_the_panel_hugs_the_left_gap() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Auto, 500, 400, 8);
        assert_eq!(a.left, 8);
    }

    #[test]
    fn zero_gap_reproduces_flush_placement() {
        let a = Anchored::resolve(ANCHOR, PanelSide::Left, 400, 1920, 0);
        assert_eq!(a.vertical, 28, "flush against the bar");
    }
}
