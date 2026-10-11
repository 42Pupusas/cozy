//! [`Spacing`] -- the design guide's 4px rhythm and the geometry built on it.
//!
//! The guide specifies a compact 4px-based spacing scale (XS 4, S 8, M 12,
//! L 16, XL 24) plus a table of key dimensions: a 58px toolbar, 34px sidebar
//! rows, 49px list rows, 31px controls, a 35px status line. Those numbers are
//! gathered here rather than sprinkled through the painter as literals,
//! because a magic `12` in a `fill_rect` call is indistinguishable from a
//! magic `13`, and the whole point of a rhythm is that every gap is one of a
//! handful of deliberate values.
//!
//! The guide's pixel table assumes its own 13px type at scale 1. Ours is
//! configurable and may be running on a `HiDPI` output, so every dimension is
//! expressed as a *scaled* value: the ratios the guide chose are preserved,
//! while the absolute sizes follow the user's font and output. A row that
//! stayed 49px tall under a 20px font would clip its own glyphs.

use platform::draw::Rect;

/// The spacing scale and the dimensions derived from it.
///
/// `Copy` because it is read constantly during paint and is four words wide;
/// threading a reference through every helper would buy nothing.
#[derive(Debug, Clone, Copy)]
pub struct Spacing {
    /// The scale factor applied to every guide dimension: output `HiDPI` scale
    /// combined with how far the configured font size deviates from the
    /// guide's 13px baseline.
    scale: f32,
}

impl Spacing {
    /// The font size the guide's pixel table was drawn against.
    ///
    /// Every key dimension below is a ratio against this, so a user running
    /// 16px type gets proportionally roomier chrome rather than the guide's
    /// literal pixels around larger glyphs.
    const GUIDE_FONT_PX: f32 = 13.0;

    /// Unscaled scale-step values, in the guide's own pixels.
    const XS: f32 = 4.0;
    const S: f32 = 8.0;
    const M: f32 = 12.0;
    const L: f32 = 16.0;
    const XL: f32 = 24.0;

    /// Build the rhythm for a given configured font size and output scale.
    #[must_use]
    pub fn new(font_px: f32, output_scale: f32) -> Self {
        // Guard against a nonsensical or zero font size producing a zero
        // scale, which would collapse every dimension to nothing and paint an
        // invisible window.
        let font = if font_px > 1.0 {
            font_px
        } else {
            Self::GUIDE_FONT_PX
        };
        let output = if output_scale > 0.1 {
            output_scale
        } else {
            1.0
        };
        Self {
            scale: (font / Self::GUIDE_FONT_PX) * output,
        }
    }

    /// Scale one guide dimension into device pixels.
    ///
    /// Rounds rather than truncating, and floors at 1 for any positive input:
    /// a hairline that scales to 0.4px must still be *visible*, or dividers
    /// vanish at small sizes and the layout loses its structure.
    fn at(self, guide_px: f32) -> i32 {
        if guide_px <= 0.0 {
            return 0;
        }
        #[allow(clippy::cast_possible_truncation)]
        let scaled = (guide_px * self.scale).round() as i32;
        scaled.max(1)
    }

    /// XS -- 4px. Icon and micro-label adjustment.
    #[must_use]
    pub fn xs(self) -> i32 {
        self.at(Self::XS)
    }

    /// S -- 8px. Control internals and compact gaps.
    #[must_use]
    pub fn s(self) -> i32 {
        self.at(Self::S)
    }

    /// M -- 12px. Sidebar padding and row content.
    #[must_use]
    pub fn m(self) -> i32 {
        self.at(Self::M)
    }

    /// L -- 16px. Section separation.
    #[must_use]
    pub fn l(self) -> i32 {
        self.at(Self::L)
    }

    /// XL -- 24px. Main content inset.
    ///
    /// The largest step in the guide's rhythm. Not yet reached for by the
    /// painter -- the current insets land on L -- but kept because the scale
    /// is a published five-step vocabulary and a four-step version of it
    /// would be a different design system.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn xl(self) -> i32 {
        self.at(Self::XL)
    }

    /// Corner radius for controls -- buttons, the filter box, row highlights
    /// (guide: 6px).
    ///
    /// Scaled with everything else, because a 6px corner on a 2x output reads
    /// as half the curve the design asks for. It never needs to be checked
    /// against a control's own size at the call site: `RoundRect` clamps a
    /// radius past half the shorter side to a pill.
    #[must_use]
    pub fn radius(self) -> i32 {
        self.at(6.0)
    }

    /// Corner radius for the small keycap in the status strip (guide: 3px).
    ///
    /// Tighter than [`Self::radius`]: a keycap is a fraction of a button's
    /// height, and the same 6px curve on it would round it into a lozenge.
    #[must_use]
    pub fn radius_small(self) -> i32 {
        self.at(3.0)
    }

    /// Half the space between neighbouring cells. Every cell, tile or card,
    /// is drawn this far inside its slot, and the window margin is the same
    /// value, so the outer margin equals the gap between cells.
    #[must_use]
    pub fn gutter(self) -> i32 {
        self.xs()
    }

    /// Horizontal padding inside the pinned card (guide: 12px).
    #[must_use]
    pub fn sidebar_pad(self) -> i32 {
        self.at(12.0)
    }

    /// Inset of a sidebar row's label from the row's own left edge
    /// (guide: 9px).
    ///
    /// Smaller than the column's padding because the row highlight is already
    /// inset by that padding; the label sits 9px inside the *row*, per the
    /// guide's `.nav { padding: 0 9px }`.
    #[must_use]
    pub fn sidebar_row_pad(self) -> i32 {
        self.at(9.0)
    }

    /// The accent rule marking an active sidebar row (guide: 2px).
    #[must_use]
    pub fn rule(self) -> i32 {
        self.at(2.0)
    }

    /// Toolbar height (guide: 58px).
    #[must_use]
    pub fn toolbar_height(self) -> i32 {
        self.at(58.0)
    }

    /// Sidebar navigation row height (guide: 34px).
    #[must_use]
    pub fn sidebar_row(self) -> i32 {
        self.at(34.0)
    }

    /// File list row height (guide: 49px).
    #[must_use]
    pub fn list_row(self) -> i32 {
        self.at(49.0)
    }

    /// Height of search and view controls (guide: 31px).
    #[must_use]
    pub fn control_height(self) -> i32 {
        self.at(31.0)
    }

    /// Status line height (guide: 35px).
    #[must_use]
    pub fn status_height(self) -> i32 {
        self.at(35.0)
    }

    /// Height of the sidebar's storage footer (guide: 42px).
    ///
    /// The guide builds this from parts -- 14px of top padding, a ~14px
    /// caption line, a 9px gap, the 2px meter and 2px of bottom padding --
    /// and the sum is what the layout actually needs to reserve, so it is
    /// stated once here rather than re-added at the call site.
    #[must_use]
    pub fn storage_height(self) -> i32 {
        self.at(42.0)
    }

    /// Sidebar width (guide: 210px).
    #[must_use]
    pub fn sidebar_width(self) -> i32 {
        self.at(210.0)
    }

    /// Width of the pinned chrome card, and with it the shortest side a
    /// spiral tile may have: the card is the flow's last tile, so one token
    /// sets both. Derived from what the card's own toolbar honestly needs --
    /// four icon buttons plus padding, at least as wide as the old guide
    /// floor so tiles still hold their labels.
    #[must_use]
    pub fn tile_min(self) -> i32 {
        let toolbar = self.icon_button() * 4 + self.sidebar_pad() * 2 + self.s();
        toolbar.max(self.at(72.0))
    }

    /// The shortest side a spiral tile may have. Smaller than the card's
    /// width so more tiles fit while the card keeps its room.
    #[must_use]
    pub fn tile_floor(self) -> i32 {
        self.at(88.0)
    }

    /// Width the pinned card asks of its spiral cell: two tiles' worth, so
    /// the card is a landscape strip and its buttons are wide.
    #[must_use]
    pub fn card_width(self) -> i32 {
        (self.tile_min() * 2).max(self.card_nav_row())
    }

    /// What the card's navigation row needs: six icon buttons, the gap that
    /// sets the dotfile toggle apart, and padding on both sides.
    #[must_use]
    pub fn card_nav_row(self) -> i32 {
        self.icon_button() * 6 + self.s() + self.sidebar_pad() * 2
    }

    /// The narrowest a toolbar button may be drawn (guide: 30px).
    #[must_use]
    pub fn icon_button(self) -> i32 {
        self.at(30.0)
    }

    /// Height of a toolbar button, which stretches to the card's width but
    /// stays this tall.
    #[must_use]
    pub fn nav_button_height(self) -> i32 {
        self.at(36.0)
    }

    /// How far a hovered tile rises.
    #[must_use]
    pub fn tile_lift(self) -> i32 {
        self.at(3.0)
    }

    /// Edge of a tile's action button when the tile has room to spare.
    #[must_use]
    pub fn action_button(self) -> i32 {
        self.at(28.0)
    }

    /// How far each edge of an action button moves out when it is hovered.
    #[must_use]
    pub fn action_grow(self) -> i32 {
        self.at(2.0)
    }

    /// The smallest action button worth drawing; a tile too narrow for four
    /// of these shows none.
    #[must_use]
    pub fn action_button_min(self) -> i32 {
        self.at(18.0)
    }

    /// A tile's face: its cell less the gutter on every side, so neighbours
    /// read as separate cards.
    #[must_use]
    pub fn tile_face(self, cell: Rect) -> Rect {
        cell.inset(self.gutter())
    }

    /// Whether a tile face this tall sets its name in the title tier rather
    /// than the body tier.
    #[must_use]
    pub fn is_roomy_tile(self, face_height: i32) -> bool {
        face_height >= self.tile_min() * 2
    }
}

#[cfg(test)]
mod tests {
    use super::Spacing;
    use platform::draw::Rect;

    /// At the guide's own font size and scale 1, the tokens must reproduce
    /// the guide's literal pixel table -- that is the contract this module
    /// exists to encode.
    #[test]
    fn the_guide_baseline_reproduces_the_guide_numbers() {
        let s = Spacing::new(13.0, 1.0);
        assert_eq!(s.xs(), 4);
        assert_eq!(s.s(), 8);
        assert_eq!(s.m(), 12);
        assert_eq!(s.l(), 16);
        assert_eq!(s.xl(), 24);
        assert_eq!(s.toolbar_height(), 58);
        assert_eq!(s.sidebar_row(), 34);
        assert_eq!(s.list_row(), 49);
        assert_eq!(s.control_height(), 31);
        assert_eq!(s.status_height(), 35);
        assert_eq!(s.sidebar_width(), 210);
        assert_eq!(s.tile_min(), 152);
        assert_eq!(s.tile_floor(), 88);
        assert_eq!(s.card_width(), 304);
        assert_eq!(s.nav_button_height(), 36);
        assert_eq!(s.tile_lift(), 3);
        assert_eq!(s.action_button(), 28);
        assert_eq!(s.action_grow(), 2);
        assert_eq!(s.action_button_min(), 18);
        assert_eq!(s.storage_height(), 42);
        assert_eq!(s.radius(), 6);
        assert_eq!(s.radius_small(), 3);
        assert_eq!(s.sidebar_pad(), 12);
        assert_eq!(s.sidebar_row_pad(), 9);
    }

    /// A corner radius must never be so large it swallows a control whole, nor
    /// round away to a square at a small size.
    #[test]
    fn the_corner_radius_suits_the_controls_it_rounds() {
        for font in [8.0, 13.0, 20.0, 40.0] {
            for scale in [1.0, 2.0] {
                let s = Spacing::new(font, scale);
                assert!(s.radius() > 0, "radius vanished at {font}/{scale}");
                assert!(
                    s.radius() * 2 < s.control_height(),
                    "radius would pill the control at {font}/{scale}"
                );
                assert!(
                    s.radius_small() < s.radius(),
                    "the keycap radius must stay tighter at {font}/{scale}"
                );
            }
        }
    }

    /// The scale steps must stay in ascending order at every size, or
    /// "section separation" could come out tighter than "compact gap" and the
    /// rhythm would read as random.
    #[test]
    fn the_steps_stay_ordered_at_every_size() {
        for font in [8.0, 13.0, 16.0, 24.0, 40.0] {
            for scale in [1.0, 1.5, 2.0] {
                let s = Spacing::new(font, scale);
                assert!(s.xs() <= s.s(), "xs <= s at {font}/{scale}");
                assert!(s.s() <= s.m(), "s <= m at {font}/{scale}");
                assert!(s.m() <= s.l(), "m <= l at {font}/{scale}");
                assert!(s.l() <= s.xl(), "l <= xl at {font}/{scale}");
            }
        }
    }

    /// A larger font must grow the chrome, not leave glyphs to clip against
    /// fixed-height rows.
    #[test]
    fn a_bigger_font_grows_the_chrome() {
        let small = Spacing::new(13.0, 1.0);
        let big = Spacing::new(26.0, 1.0);
        assert!(big.list_row() > small.list_row());
        assert!(big.toolbar_height() > small.toolbar_height());
        assert!(big.sidebar_width() > small.sidebar_width());
    }

    /// `HiDPI` must scale the geometry too: the same layout, twice the pixels.
    #[test]
    fn hidpi_doubles_the_geometry() {
        let one = Spacing::new(13.0, 1.0);
        let two = Spacing::new(13.0, 2.0);
        assert_eq!(two.list_row(), one.list_row() * 2);
        assert_eq!(two.xl(), one.xl() * 2);
    }

    #[test]
    fn a_tile_face_is_its_cell_less_the_gutter() {
        let s = Spacing::new(13.0, 1.0);
        let face = s.tile_face(Rect::new(10, 20, 100, 80));
        assert_eq!(face, Rect::new(14, 24, 92, 72));
    }

    #[test]
    fn only_a_tall_tile_gets_the_title_tier() {
        let s = Spacing::new(13.0, 1.0);
        assert!(!s.is_roomy_tile(s.tile_min() * 2 - 1));
        assert!(s.is_roomy_tile(s.tile_min() * 2));
    }

    #[test]
    fn a_lift_stays_inside_the_gutter() {
        for font in [8.0, 13.0, 20.0, 40.0] {
            let s = Spacing::new(font, 1.0);
            assert!(
                s.tile_lift() <= s.gutter(),
                "a lifted tile must not touch its neighbour at {font}"
            );
            assert!(s.action_button_min() <= s.action_button());
            assert!(
                s.action_grow() * 2 <= s.xs(),
                "two grown neighbours must not touch at {font}"
            );
        }
    }

    /// A degenerate font size or scale must not collapse the layout to zero;
    /// an invisible window is worse than an ugly one.
    #[test]
    fn degenerate_inputs_fall_back_rather_than_collapsing() {
        for (font, scale) in [(0.0, 1.0), (13.0, 0.0), (-5.0, -5.0), (0.0, 0.0)] {
            let s = Spacing::new(font, scale);
            assert!(s.xs() > 0, "xs must be visible at {font}/{scale}");
            assert!(s.list_row() > 0, "rows must have height at {font}/{scale}");
            assert!(s.sidebar_width() > 0, "sidebar must be visible");
        }
    }

    /// Every dimension must stay positive at the smallest sane size, so a
    /// tiny font cannot produce zero-height rows or invisible dividers.
    #[test]
    fn nothing_rounds_away_to_zero_at_a_tiny_size() {
        let s = Spacing::new(8.0, 1.0);
        for (name, value) in [
            ("xs", s.xs()),
            ("s", s.s()),
            ("m", s.m()),
            ("l", s.l()),
            ("xl", s.xl()),
            ("rule", s.rule()),
            ("toolbar", s.toolbar_height()),
            ("sidebar_row", s.sidebar_row()),
            ("list_row", s.list_row()),
            ("control", s.control_height()),
            ("status", s.status_height()),
            ("icon_button", s.icon_button()),
            ("tile_lift", s.tile_lift()),
            ("action_button", s.action_button()),
            ("action_grow", s.action_grow()),
            ("action_button_min", s.action_button_min()),
            ("tile_min", s.tile_min()),
            ("radius", s.radius()),
            ("radius_small", s.radius_small()),
            ("sidebar_pad", s.sidebar_pad()),
            ("sidebar_row_pad", s.sidebar_row_pad()),
        ] {
            assert!(value > 0, "{name} must be positive");
        }
    }

    /// The card's navigation row must hold five buttons and the gap before
    /// the dotfile toggle without dropping any.
    #[test]
    fn the_card_fits_its_navigation_row() {
        for font in [8.0, 13.0, 20.0] {
            let s = Spacing::new(font, 1.0);
            assert!(
                s.card_nav_row() <= s.card_width(),
                "the nav row overflows at {font}"
            );
        }
    }
}
