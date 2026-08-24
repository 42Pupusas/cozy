//! [`Toolbar`] -- where every control in the top bar begins and ends.
//!
//! # Why this exists
//!
//! The toolbar's geometry used to live as local variables inside the painter's
//! `paint_nav_buttons`, `paint_filter_control` and `paint_view_toggle`. That is
//! fine for exactly as long as the controls are decorative: the moment one of
//! them has to respond to a click, the hit-test needs the same rectangles the
//! paint used, and the only ways to get them are to duplicate the arithmetic
//! (which then silently drifts, so the clickable region creeps away from the
//! drawn button) or to lift it out here. This is the lift.
//!
//! So this type is the single answer to "where is the back button", asked by
//! both the painter and the pointer handler. It is pure arithmetic over a
//! rectangle -- no canvas, no events -- so the whole layout is testable, and
//! the tests can assert the thing that actually matters: that what you can
//! click is exactly what you can see.
//!
//! # The dropping rule
//!
//! In a narrow window the controls are *dropped* rather than overlapped, in
//! priority order: the path outranks the filter box, which outranks the view
//! toggle. A dropped control returns `None` from its accessor, and
//! [`Self::hit`] then cannot report it -- which is the property that keeps an
//! invisible button from swallowing clicks meant for the breadcrumb.

use platform::draw::Rect;

use crate::spacing::Spacing;

/// A control in the toolbar, as identified by a pointer position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The back chevron.
    Back,
    /// The forward chevron.
    Forward,
    /// The dotfile visibility toggle.
    ToggleHidden,
    /// The filter box -- clicking it focuses it.
    Filter,
    /// The list segment of the view toggle.
    ViewList,
    /// The grid segment of the view toggle.
    ViewGrid,
}

/// The resolved geometry of the toolbar's controls.
///
/// Built per paint and per pointer event from the toolbar band. Cheap enough
/// (a dozen additions) that caching it would only create a chance for it to go
/// stale against a resize -- the same reasoning as [`crate::frame::Frame`].
#[derive(Debug, Clone, Copy)]
pub struct Toolbar {
    /// The toolbar band itself.
    bar: Rect,
    spacing: Spacing,
    /// Height of the boxed controls (filter box, view toggle).
    control_height: i32,
    /// Current width of the filter box, which grows when focused.
    search_width: i32,
}

impl Toolbar {
    /// Resolve the layout for the toolbar band `bar`.
    ///
    /// `filtering` is whether the filter box has focus, which widens it per
    /// the guide -- taken as a parameter rather than read from the browser so
    /// this type stays free of the state machine.
    #[must_use]
    pub fn new(bar: Rect, spacing: Spacing, control_height: i32, filtering: bool) -> Self {
        let search_width = if filtering {
            spacing.search_width_focused()
        } else {
            spacing.search_width()
        };
        Self {
            bar,
            spacing,
            control_height,
            search_width,
        }
    }

    /// The back chevron's square, or `None` if it would not fit.
    #[must_use]
    pub fn back(&self) -> Option<Rect> {
        self.nav_button(0)
    }

    /// The forward chevron's square, or `None` if it would not fit.
    #[must_use]
    pub fn forward(&self) -> Option<Rect> {
        self.nav_button(1)
    }

    /// The `index`th navigation square, counting from the left edge.
    ///
    /// The two sit flush against each other with no gap: they read as one
    /// paired control, which is what the guide draws.
    fn nav_button(&self, index: i32) -> Option<Rect> {
        let size = self.spacing.icon_button();
        let x = self.bar.x + self.spacing.toolbar_pad() + size * index;
        // Never let the buttons crowd out the path itself, which is the one
        // thing in the toolbar the user actually needs.
        if x + size > self.bar.right() {
            return None;
        }
        Some(Rect::new(x, self.centered_y(size), size, size))
    }

    /// The dotfile visibility toggle, or `None` if it would not fit.
    ///
    /// Set apart from the chevron pair by a gap rather than drawn flush with
    /// it. The chevrons are *actions* that move you somewhere; this is a
    /// *state* that changes what you are looking at. Making three buttons
    /// share one outline would claim they are the same kind of thing, and the
    /// user would reasonably expect the third to navigate too.
    #[must_use]
    pub fn hidden_toggle(&self) -> Option<Rect> {
        let size = self.spacing.icon_button();
        let x = self.bar.x + self.spacing.toolbar_pad() + size * 2 + self.spacing.s();
        if x + size > self.bar.right() {
            return None;
        }
        Some(Rect::new(x, self.centered_y(size), size, size))
    }

    /// Where the breadcrumb path may start: after the left-hand buttons.
    ///
    /// Reserves the toggle's width whether or not the toggle is drawn. The
    /// path must not slide left the instant the window narrows past the
    /// toggle's threshold -- and reserving unconditionally also keeps this
    /// free of the mutual recursion it would otherwise have with the
    /// right-hand controls, which test their own fit against this number.
    #[must_use]
    pub fn breadcrumb_x(&self) -> i32 {
        self.bar.x
            + self.spacing.toolbar_pad()
            + self.spacing.icon_button() * 3
            + self.spacing.s() * 2
    }

    /// How many pixels the breadcrumb may occupy before it would collide with
    /// the controls at the right end.
    #[must_use]
    pub fn breadcrumb_width(&self) -> i32 {
        // Reserved from the *resting* search width, deliberately: reserving the
        // focused width would make the whole path jump sideways every time the
        // user clicked into the filter box.
        let reserved = self.spacing.search_width()
            + self.spacing.view_button() * 2
            + self.spacing.toolbar_pad()
            + self.spacing.s() * 2;
        self.bar.right() - self.breadcrumb_x() - reserved
    }

    /// The filter box, or `None` when the window is too narrow for it.
    #[must_use]
    pub fn filter(&self) -> Option<Rect> {
        let x =
            self.bar.right() - self.spacing.toolbar_pad() - self.toggle_span() - self.search_width;
        // Guard against a column too narrow to hold the control at all --
        // drawing it at a negative x would wrap around the canvas.
        if x < self.breadcrumb_x() {
            return None;
        }
        Some(Rect::new(
            x,
            self.centered_y(self.control_height),
            self.search_width,
            self.control_height,
        ))
    }

    /// The view toggle's outline, or `None` when there is no room.
    #[must_use]
    pub fn view_toggle(&self) -> Option<Rect> {
        let width = self.spacing.view_button() * 2;
        let x = self.bar.right() - self.spacing.toolbar_pad() - width;
        // Dropped entirely rather than overlapped when the toolbar is too
        // tight: the filter box and the path outrank it.
        if x <= self.breadcrumb_x() {
            return None;
        }
        Some(Rect::new(
            x,
            self.centered_y(self.control_height),
            width,
            self.control_height,
        ))
    }

    /// One segment of the view toggle: `index` 0 is list, 1 is grid.
    #[must_use]
    pub fn view_segment(&self, index: i32) -> Option<Rect> {
        let outline = self.view_toggle()?;
        let button = self.spacing.view_button();
        Some(Rect::new(
            outline.x + button * index,
            outline.y,
            button,
            outline.h,
        ))
    }

    /// Width the view toggle claims at the right end, including its gap.
    ///
    /// Reserved by the filter box whether or not the toggle is actually drawn,
    /// so that the filter box does not slide right as the window narrows past
    /// the toggle's threshold.
    fn toggle_span(&self) -> i32 {
        self.spacing.view_button() * 2 + self.spacing.s()
    }

    /// Vertically centre something `height` tall within the band.
    const fn centered_y(&self, height: i32) -> i32 {
        self.bar.y + (self.bar.h - height) / 2
    }

    /// Which control is at `(x, y)`, if any.
    ///
    /// Only reports controls that are actually drawn, because it asks the same
    /// accessors the painter does -- so a control dropped for want of room
    /// cannot swallow a click.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Target> {
        if !Self::contains(self.bar, x, y) {
            return None;
        }
        // The segments are tested before the toggle's outline they sit inside,
        // and the outline is never itself a target, so a click always resolves
        // to one segment or the other rather than to the pair.
        let candidates = [
            (self.back(), Target::Back),
            (self.forward(), Target::Forward),
            (self.hidden_toggle(), Target::ToggleHidden),
            (self.filter(), Target::Filter),
            (self.view_segment(0), Target::ViewList),
            (self.view_segment(1), Target::ViewGrid),
        ];
        candidates
            .into_iter()
            .find(|(rect, _)| rect.is_some_and(|r| Self::contains(r, x, y)))
            .map(|(_, target)| target)
    }

    /// Whether `rect` covers `(x, y)`.
    ///
    /// Half-open on the far edges, so two flush controls never both claim the
    /// pixel column they share.
    const fn contains(rect: Rect, x: i32, y: i32) -> bool {
        x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::{Target, Toolbar};
    use crate::spacing::Spacing;
    use platform::draw::Rect;

    /// A toolbar at the guide's baseline, in a window `width` wide.
    fn toolbar_of(width: i32, filtering: bool) -> Toolbar {
        let spacing = Spacing::new(13.0, 1.0);
        let bar = Rect::new(0, 0, width, spacing.toolbar_height());
        Toolbar::new(bar, spacing, spacing.control_height(), filtering)
    }

    /// The centre of `rect`, which is where a user aiming at a control clicks.
    fn centre(rect: Rect) -> (i32, i32) {
        (rect.x + rect.w / 2, rect.y + rect.h / 2)
    }

    #[test]
    fn the_two_chevrons_sit_side_by_side_at_the_left_edge() {
        let bar = toolbar_of(1200, false);
        let (back, forward) = (bar.back().expect("back fits"), bar.forward().expect("fwd"));
        assert_eq!(
            back.right(),
            forward.x,
            "the pair reads as one control, so they share an edge"
        );
        assert!(back.x > 0, "the first button must clear the window edge");
        assert_eq!(back.h, forward.h, "the pair must be the same size");
    }

    #[test]
    fn every_control_stays_inside_the_band() {
        let bar = toolbar_of(1200, false);
        for rect in [
            bar.back().expect("back"),
            bar.forward().expect("forward"),
            bar.hidden_toggle().expect("hidden"),
            bar.filter().expect("filter"),
            bar.view_toggle().expect("toggle"),
        ] {
            assert!(rect.y >= 0, "a control must not poke out of the top");
            assert!(
                rect.bottom() <= bar.bar.bottom(),
                "a control must not poke out of the bottom"
            );
            assert!(
                rect.right() <= bar.bar.right(),
                "a control must not run past the right edge"
            );
        }
    }

    #[test]
    fn the_controls_never_overlap_each_other() {
        for filtering in [false, true] {
            let bar = toolbar_of(1200, filtering);
            let rects = [
                bar.back().expect("back"),
                bar.forward().expect("forward"),
                bar.hidden_toggle().expect("hidden"),
                bar.filter().expect("filter"),
                bar.view_toggle().expect("toggle"),
            ];
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    let overlaps = a.x < b.right() && b.x < a.right();
                    assert!(
                        !overlaps,
                        "controls must not overlap: {a:?} and {b:?} (filtering={filtering})"
                    );
                }
            }
        }
    }

    #[test]
    fn the_filter_sits_left_of_the_view_toggle() {
        let bar = toolbar_of(1200, false);
        let filter = bar.filter().expect("filter");
        let toggle = bar.view_toggle().expect("toggle");
        assert!(
            filter.right() <= toggle.x,
            "the guide puts the filter box before the toggle"
        );
    }

    #[test]
    fn focusing_the_filter_widens_it_leftwards() {
        let (resting, focused) = (toolbar_of(1200, false), toolbar_of(1200, true));
        let (a, b) = (resting.filter().expect("a"), focused.filter().expect("b"));
        assert!(b.w > a.w, "the guide widens the box on focus");
        assert_eq!(
            a.right(),
            b.right(),
            "it must grow towards the path, not push the toggle off the edge"
        );
    }

    #[test]
    fn the_path_does_not_shift_when_the_filter_takes_focus() {
        // Reserving the focused width would make the whole breadcrumb jump
        // sideways the instant the user clicked into the box.
        let (resting, focused) = (toolbar_of(1200, false), toolbar_of(1200, true));
        assert_eq!(resting.breadcrumb_x(), focused.breadcrumb_x());
        assert_eq!(resting.breadcrumb_width(), focused.breadcrumb_width());
    }

    #[test]
    fn the_breadcrumb_starts_after_the_chevrons() {
        let bar = toolbar_of(1200, false);
        assert!(
            bar.breadcrumb_x() >= bar.forward().expect("forward").right(),
            "the path must not be drawn over the navigation buttons"
        );
        assert!(
            bar.breadcrumb_x() >= bar.hidden_toggle().expect("hidden").right(),
            "nor over the hidden-files toggle"
        );
    }

    /// The chevrons act; the toggle changes what is shown. A gap says so.
    #[test]
    fn the_hidden_toggle_is_set_apart_from_the_chevron_pair() {
        let bar = toolbar_of(1200, false);
        let forward = bar.forward().expect("forward");
        let hidden = bar.hidden_toggle().expect("hidden");
        assert!(
            hidden.x > forward.right(),
            "a state control must not read as a third chevron"
        );
        assert_eq!(hidden.w, forward.w, "but it is the same size button");
        assert_eq!(hidden.y, forward.y, "and sits on the same baseline");
    }

    #[test]
    fn a_click_on_the_hidden_toggle_reports_it() {
        let bar = toolbar_of(1200, false);
        let (x, y) = centre(bar.hidden_toggle().expect("hidden"));
        assert_eq!(bar.hit(x, y), Some(Target::ToggleHidden));
    }

    /// Toggling dotfiles does not move any control, so the button cannot
    /// slide out from under the pointer that is repeatedly clicking it.
    #[test]
    fn the_layout_does_not_depend_on_the_hidden_state() {
        let bar = toolbar_of(1200, false);
        assert_eq!(bar.hidden_toggle(), bar.hidden_toggle());
    }

    #[test]
    fn a_click_on_a_chevron_reports_it() {
        let bar = toolbar_of(1200, false);
        let (x, y) = centre(bar.back().expect("back"));
        assert_eq!(bar.hit(x, y), Some(Target::Back));
        let (x, y) = centre(bar.forward().expect("forward"));
        assert_eq!(bar.hit(x, y), Some(Target::Forward));
    }

    #[test]
    fn a_click_on_the_filter_box_reports_it() {
        let bar = toolbar_of(1200, false);
        let (x, y) = centre(bar.filter().expect("filter"));
        assert_eq!(bar.hit(x, y), Some(Target::Filter));
    }

    #[test]
    fn each_toggle_segment_is_hit_separately() {
        let bar = toolbar_of(1200, false);
        let (x, y) = centre(bar.view_segment(0).expect("list"));
        assert_eq!(bar.hit(x, y), Some(Target::ViewList));
        let (x, y) = centre(bar.view_segment(1).expect("grid"));
        assert_eq!(bar.hit(x, y), Some(Target::ViewGrid));
    }

    #[test]
    fn the_two_toggle_segments_tile_the_outline_without_overlapping() {
        let bar = toolbar_of(1200, false);
        let outline = bar.view_toggle().expect("toggle");
        let (list, grid) = (
            bar.view_segment(0).expect("list"),
            bar.view_segment(1).expect("grid"),
        );
        assert_eq!(list.x, outline.x, "the pair starts where the outline does");
        assert_eq!(list.right(), grid.x, "the segments share an edge");
        assert_eq!(
            grid.right(),
            outline.right(),
            "the pair must exactly fill the outline"
        );
    }

    #[test]
    fn the_gap_between_the_chevrons_and_the_path_hits_nothing() {
        let bar = toolbar_of(1200, false);
        let x = bar.forward().expect("forward").right() + 1;
        let y = bar.bar.h / 2;
        assert_eq!(
            bar.hit(x, y),
            None,
            "empty toolbar space must not resolve to a control"
        );
    }

    #[test]
    fn the_breadcrumb_area_hits_nothing() {
        // Clicking a path segment to navigate is deliberately not wired yet;
        // until it is, the path must not report a neighbouring control.
        let bar = toolbar_of(1200, false);
        let y = bar.bar.h / 2;
        for x in [bar.breadcrumb_x(), bar.breadcrumb_x() + 40] {
            assert_eq!(bar.hit(x, y), None, "the path is not a control yet");
        }
    }

    #[test]
    fn a_click_outside_the_band_hits_nothing() {
        let bar = toolbar_of(1200, false);
        let (x, _) = centre(bar.back().expect("back"));
        assert_eq!(
            bar.hit(x, bar.bar.bottom() + 1),
            None,
            "a click below the toolbar belongs to the list, not to a button"
        );
        assert_eq!(bar.hit(-1, bar.bar.h / 2), None);
    }

    #[test]
    fn a_control_dropped_for_want_of_room_cannot_be_clicked() {
        // The invariant that makes narrow windows safe: if it is not drawn, it
        // must not swallow clicks meant for whatever is still visible.
        let wide = toolbar_of(1200, false);
        let toggle = wide.view_toggle().expect("wide enough to draw a toggle");
        let (x, y) = centre(toggle);
        // Narrow enough that the toggle's left edge would cross the path's
        // start: 240px still fits it, so the drop threshold is lower than the
        // width at which the *filter* box goes.
        let narrow = toolbar_of(150, false);
        assert!(
            narrow.view_toggle().is_none(),
            "a 150px toolbar has no room for the toggle"
        );
        assert_eq!(
            narrow.hit(x, y),
            None,
            "an undrawn control must not be clickable"
        );
    }

    #[test]
    fn a_narrow_toolbar_keeps_the_navigation_buttons() {
        // Priority order: the chevrons are the leftmost thing and the cheapest
        // to keep, so they survive when the right-hand controls are dropped.
        let bar = toolbar_of(240, false);
        assert!(bar.back().is_some(), "back must survive a narrow window");
        assert!(bar.forward().is_some(), "forward must survive too");
        assert!(bar.hidden_toggle().is_some(), "and so must the toggle");
        assert!(bar.filter().is_none(), "the filter box has to go");
    }

    #[test]
    fn a_degenerate_toolbar_yields_no_controls_rather_than_negative_geometry() {
        let bar = toolbar_of(0, false);
        assert!(bar.back().is_none(), "nothing fits in a zero-width toolbar");
        assert!(bar.hidden_toggle().is_none());
        assert!(bar.filter().is_none());
        assert!(bar.view_toggle().is_none());
        assert_eq!(bar.hit(0, 0), None, "and nothing can be clicked");
    }

    #[test]
    fn the_hit_regions_are_exactly_the_drawn_rectangles() {
        // The whole point of this type: what you can click is what you can see.
        // Every pixel of a control resolves to it, and the pixel just past its
        // far edge does not.
        let bar = toolbar_of(1200, false);
        for (rect, target) in [
            (bar.back().expect("back"), Target::Back),
            (bar.forward().expect("forward"), Target::Forward),
            (bar.hidden_toggle().expect("hidden"), Target::ToggleHidden),
            (bar.filter().expect("filter"), Target::Filter),
            (bar.view_segment(0).expect("list"), Target::ViewList),
            (bar.view_segment(1).expect("grid"), Target::ViewGrid),
        ] {
            for (x, y) in [
                (rect.x, rect.y),
                (rect.right() - 1, rect.bottom() - 1),
                (rect.x, rect.bottom() - 1),
                (rect.right() - 1, rect.y),
            ] {
                assert_eq!(
                    bar.hit(x, y),
                    Some(target),
                    "corner ({x},{y}) of {target:?} must resolve to it"
                );
            }
            assert_ne!(
                bar.hit(rect.right(), rect.y + rect.h / 2),
                Some(target),
                "the column past {target:?}'s right edge belongs to its neighbour"
            );
        }
    }

    #[test]
    fn an_offset_band_moves_every_control_with_it() {
        // The toolbar lives inside the right column, so its band does not start
        // at the window's origin -- geometry must be relative to the band.
        let spacing = Spacing::new(13.0, 1.0);
        let bar = Rect::new(210, 0, 990, spacing.toolbar_height());
        let shifted = Toolbar::new(bar, spacing, spacing.control_height(), false);
        let back = shifted.back().expect("back");
        assert!(
            back.x >= 210,
            "the back button must sit inside the right column, not under the sidebar"
        );
        let (x, y) = centre(back);
        assert_eq!(shifted.hit(x, y), Some(Target::Back));
        assert_eq!(
            shifted.hit(100, y),
            None,
            "a click in the sidebar is not a toolbar click"
        );
    }

    #[test]
    fn a_hidpi_toolbar_scales_without_losing_a_control() {
        let spacing = Spacing::new(13.0, 2.0);
        let bar = Rect::new(0, 0, 2400, spacing.toolbar_height());
        let hidpi = Toolbar::new(bar, spacing, spacing.control_height(), false);
        let back = hidpi.back().expect("back");
        let plain = toolbar_of(1200, false).back().expect("back");
        assert!(
            back.w > plain.w,
            "a doubled output scale must double the button"
        );
        let (x, y) = centre(hidpi.filter().expect("filter"));
        assert_eq!(hidpi.hit(x, y), Some(Target::Filter));
    }
}
