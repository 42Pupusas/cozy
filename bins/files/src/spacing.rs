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

    /// A one-device-pixel hairline, never scaled away to nothing.
    ///
    /// Deliberately *not* scaled: the guide's dividers are 1px rules, and a
    /// 2px border at scale 2 reads as a heavy line rather than a hairline.
    /// It still hangs off `Spacing` rather than being a free constant so the
    /// call sites read as one vocabulary -- `spacing.hairline()` next to
    /// `spacing.m()` -- and so it can start scaling later without touching
    /// them.
    #[must_use]
    #[allow(clippy::unused_self)]
    pub const fn hairline(self) -> i32 {
        1
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

    /// Column-header row height (guide: 30px).
    #[must_use]
    pub fn column_header(self) -> i32 {
        self.at(30.0)
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

    /// Thickness of the storage meter's bar (guide: 2px).
    ///
    /// Numerically the same as [`Self::rule`] today, but named separately
    /// because they are different things: one is a selection marker, the
    /// other a data display. Collapsing them would couple a restyle of the
    /// active-row rule to the thickness of a progress bar.
    #[must_use]
    pub fn meter_height(self) -> i32 {
        self.at(2.0)
    }

    /// Sidebar width (guide: 210px).
    #[must_use]
    pub fn sidebar_width(self) -> i32 {
        self.at(210.0)
    }

    /// Collapsed sidebar rail width (guide: 58px).
    #[must_use]
    pub fn rail_width(self) -> i32 {
        self.at(58.0)
    }

    /// The window width below which the sidebar collapses to a rail
    /// (guide: 780px).
    #[must_use]
    pub fn collapse_below(self) -> i32 {
        self.at(780.0)
    }

    /// Width of a toolbar icon button (guide: 30px square).
    ///
    /// Unused until the toolbar's buttons are drawn -- iconography is a
    /// separate, deliberately deferred piece of work -- but the dimension
    /// belongs with the rest of the guide's table rather than being
    /// rediscovered later.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn icon_button(self) -> i32 {
        self.at(30.0)
    }

    /// Width of one view-toggle button (guide: 31px square).
    #[must_use]
    pub fn view_button(self) -> i32 {
        self.at(31.0)
    }

    /// Resting width of the filter control (guide: 200px).
    #[must_use]
    pub fn search_width(self) -> i32 {
        self.at(200.0)
    }

    /// Focused width of the filter control (guide: 245px).
    #[must_use]
    pub fn search_width_focused(self) -> i32 {
        self.at(245.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Spacing;

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
        assert_eq!(s.rail_width(), 58);
        assert_eq!(s.storage_height(), 42);
        assert_eq!(s.meter_height(), 2);
        assert_eq!(s.view_button(), 31);
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

    /// Hairlines stay one device pixel regardless of scale -- a scaled-up
    /// divider reads as a heavy border, not a hairline.
    #[test]
    fn a_hairline_is_always_one_pixel() {
        assert_eq!(Spacing::new(13.0, 1.0).hairline(), 1);
        assert_eq!(Spacing::new(40.0, 3.0).hairline(), 1);
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
            ("column_header", s.column_header()),
            ("icon_button", s.icon_button()),
            ("rail", s.rail_width()),
        ] {
            assert!(value > 0, "{name} must be positive");
        }
    }

    /// The focused filter box must actually be wider than at rest, or the
    /// guide's grow-on-focus affordance is invisible.
    #[test]
    fn the_filter_grows_when_focused() {
        let s = Spacing::new(13.0, 1.0);
        assert!(s.search_width_focused() > s.search_width());
    }

    /// The collapse threshold must exceed the expanded sidebar itself,
    /// otherwise the rail would never engage before the list ran out of room.
    #[test]
    fn the_collapse_threshold_leaves_room_for_a_list() {
        let s = Spacing::new(13.0, 1.0);
        assert!(s.collapse_below() > s.sidebar_width() * 2);
        assert!(s.rail_width() < s.sidebar_width());
    }
}
