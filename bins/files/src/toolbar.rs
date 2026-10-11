//! [`Toolbar`] -- where every control in the pinned card begins and ends.
//!
//! The single answer to "where is the back button", asked by both the painter
//! and the pointer handler, so what can be clicked is exactly what is drawn.
//! A control that does not fit returns `None`, and [`Toolbar::hit`] then
//! cannot report it.

use platform::draw::Rect;

use tiles::pinned_card::PinnedCard;
use tiles::spacing::Spacing;

/// A control in the card, as identified by a pointer position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Back,
    Forward,
    Up,
    Home,
    NewFolder,
    ToggleHidden,
    FilterBox,
}

/// The resolved geometry of the card's controls.
#[derive(Debug, Clone, Copy)]
pub struct Toolbar {
    card: PinnedCard,
    spacing: Spacing,
    control_height: i32,
}

impl Toolbar {
    #[must_use]
    pub const fn new(card: PinnedCard, spacing: Spacing, control_height: i32) -> Self {
        Self {
            card,
            spacing,
            control_height,
        }
    }

    #[must_use]
    pub fn back(&self) -> Option<Rect> {
        self.nav_button(0, 0)
    }

    #[must_use]
    pub fn forward(&self) -> Option<Rect> {
        self.nav_button(1, 0)
    }

    #[must_use]
    pub fn up(&self) -> Option<Rect> {
        self.nav_button(2, 0)
    }

    #[must_use]
    pub fn home(&self) -> Option<Rect> {
        self.nav_button(3, 0)
    }

    #[must_use]
    pub fn new_folder(&self) -> Option<Rect> {
        self.nav_button(4, 0)
    }

    /// The dotfile toggle, set apart from the navigation buttons by a gap
    /// because it changes what is shown rather than where you are.
    #[must_use]
    pub fn hidden_toggle(&self) -> Option<Rect> {
        self.nav_button(5, self.spacing.s())
    }

    /// The six buttons share the padded row equally, less the gap before the
    /// toggle; the toggle takes any rounding remainder so the row ends flush.
    fn nav_button(&self, index: i32, gap: i32) -> Option<Rect> {
        const COUNT: i32 = 6;
        let row = self.card.nav();
        let inner = row.w - self.card.pad() * 2;
        let width = (inner - self.spacing.s()) / COUNT;
        if width < self.spacing.icon_button() {
            return None;
        }
        let landscape = (width * 4 / 5).max(self.spacing.nav_button_height());
        let height = (row.h - self.spacing.s()).min(landscape);
        if height < self.spacing.nav_button_height() {
            return None;
        }
        let x = row.x + self.card.pad() + width * index + gap;
        let w = if index == COUNT - 1 {
            row.x + self.card.pad() + inner - x
        } else {
            width
        };
        let rect = Rect::new(x, Self::centered_y(row, height), w, height);
        self.fits_in_row(rect).then_some(rect)
    }

    /// The breadcrumb line.
    #[must_use]
    pub const fn breadcrumbs(&self) -> Rect {
        self.card.crumbs()
    }

    /// The filter box, spanning the card's padded width.
    #[must_use]
    pub fn filter(&self) -> Option<Rect> {
        let row = self.card.filter_row();
        let height = (row.h - self.spacing.m()).max(self.control_height);
        let rect = Rect::new(
            row.x + self.card.pad(),
            Self::centered_y(row, height),
            row.w - self.card.pad() * 2,
            height,
        );
        (rect.w > 0 && rect.y >= row.y && rect.bottom() <= row.bottom()).then_some(rect)
    }

    const fn fits_in_row(&self, rect: Rect) -> bool {
        let row = self.card.nav();
        rect.x >= row.x + self.card.pad()
            && rect.right() <= row.right() - self.card.pad()
            && rect.y >= row.y
            && rect.bottom() <= row.bottom()
    }

    const fn centered_y(row: Rect, height: i32) -> i32 {
        row.y + (row.h - height) / 2
    }

    /// Which control is at `(x, y)`, if any. Only drawn controls are
    /// reported, because this asks the same accessors the painter does.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Target> {
        [
            (self.back(), Target::Back),
            (self.forward(), Target::Forward),
            (self.up(), Target::Up),
            (self.home(), Target::Home),
            (self.new_folder(), Target::NewFolder),
            (self.hidden_toggle(), Target::ToggleHidden),
            (self.filter(), Target::FilterBox),
        ]
        .into_iter()
        .find(|(rect, _)| rect.is_some_and(|r| Self::contains(r, x, y)))
        .map(|(_, target)| target)
    }

    const fn contains(rect: Rect, x: i32, y: i32) -> bool {
        x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom()
    }
}

#[cfg(test)]
mod tests {
    use super::{Target, Toolbar};
    use tiles::pinned_card::{CardMetrics, PinnedCard};
    use tiles::spacing::Spacing;
    use platform::draw::Rect;

    fn toolbar_of(width: i32, scale: f32) -> Toolbar {
        let spacing = Spacing::new(13.0, scale);
        let metrics = CardMetrics {
            nav: spacing.toolbar_height(),
            crumbs: spacing.sidebar_row(),
            filter: spacing.control_height() + spacing.m(),
            storage: spacing.storage_height(),
        };
        let natural = metrics.nav + metrics.crumbs + metrics.filter + metrics.storage;
        let card = PinnedCard::new(Rect::new(12, 12, width, natural), spacing, metrics);
        Toolbar::new(card, spacing, spacing.control_height())
    }

    fn centre(rect: Rect) -> (i32, i32) {
        (rect.x + rect.w / 2, rect.y + rect.h / 2)
    }

    fn all(bar: &Toolbar) -> Vec<(Rect, Target)> {
        [
            (bar.back(), Target::Back),
            (bar.forward(), Target::Forward),
            (bar.up(), Target::Up),
            (bar.home(), Target::Home),
            (bar.new_folder(), Target::NewFolder),
            (bar.hidden_toggle(), Target::ToggleHidden),
            (bar.filter(), Target::FilterBox),
        ]
        .into_iter()
        .map(|(rect, target)| (rect.expect("a full-width card draws every control"), target))
        .collect()
    }

    #[test]
    fn the_navigation_row_ends_flush_with_the_padding() {
        let bar = toolbar_of(Spacing::new(13.0, 1.0).card_width(), 1.0);
        let toggle = bar.hidden_toggle().expect("toggle");
        let card = bar.card.bounds();
        assert_eq!(toggle.right(), card.right() - bar.card.pad());
    }

    #[test]
    fn the_buttons_are_wider_than_they_are_tall() {
        let bar = toolbar_of(Spacing::new(13.0, 1.0).card_width(), 1.0);
        let back = bar.back().expect("back");
        assert!(back.w > back.h, "{back:?}");
    }

    #[test]
    fn a_guide_width_card_draws_every_control() {
        let bar = toolbar_of(Spacing::new(13.0, 1.0).card_width(), 1.0);
        assert_eq!(all(&bar).len(), 7);
    }

    #[test]
    fn the_navigation_buttons_share_edges() {
        let bar = toolbar_of(256, 1.0);
        let (back, forward, up) = (
            bar.back().expect("back"),
            bar.forward().expect("forward"),
            bar.up().expect("up"),
        );
        assert_eq!(back.right(), forward.x);
        assert_eq!(forward.right(), up.x);
    }

    #[test]
    fn home_follows_up_and_the_hidden_toggle_is_set_apart() {
        let bar = toolbar_of(256, 1.0);
        let home = bar.home().expect("home");
        assert_eq!(home.x, bar.up().expect("up").right());
        assert!(bar.hidden_toggle().expect("hidden").x > home.right());
    }

    #[test]
    fn new_folder_follows_home_and_precedes_the_toggle() {
        let bar = toolbar_of(256, 1.0);
        let new_folder = bar.new_folder().expect("new folder");
        assert_eq!(new_folder.x, bar.home().expect("home").right());
        assert!(bar.hidden_toggle().expect("hidden").x > new_folder.right());
    }

    #[test]
    fn the_controls_never_overlap() {
        let bar = toolbar_of(256, 1.0);
        let rects = all(&bar);
        for (i, (a, _)) in rects.iter().enumerate() {
            for (b, _) in &rects[i + 1..] {
                let overlaps =
                    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom();
                assert!(!overlaps, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_filter_sits_between_the_crumbs_and_the_footer() {
        let bar = toolbar_of(256, 1.0);
        let filter = bar.filter().expect("filter");
        assert!(filter.y >= bar.breadcrumbs().bottom());
        let footer = bar.card.storage().expect("footer");
        assert!(filter.bottom() <= footer.y);
    }

    #[test]
    fn the_hit_regions_are_exactly_the_drawn_rectangles() {
        let bar = toolbar_of(256, 1.0);
        for (rect, target) in all(&bar) {
            for (x, y) in [
                (rect.x, rect.y),
                (rect.right() - 1, rect.bottom() - 1),
                centre(rect),
            ] {
                assert_eq!(bar.hit(x, y), Some(target), "({x},{y}) of {target:?}");
            }
            assert_ne!(bar.hit(rect.right(), rect.y + rect.h / 2), Some(target));
        }
    }

    #[test]
    fn the_breadcrumb_line_hits_nothing() {
        let bar = toolbar_of(256, 1.0);
        let (x, y) = centre(bar.breadcrumbs());
        assert_eq!(bar.hit(x, y), None);
    }

    #[test]
    fn an_undrawn_control_cannot_be_clicked() {
        let wide = toolbar_of(256, 1.0);
        let (x, y) = centre(wide.hidden_toggle().expect("hidden"));
        let narrow = toolbar_of(80, 1.0);
        assert!(narrow.hidden_toggle().is_none());
        assert_eq!(narrow.hit(x, y), None);
    }

    #[test]
    fn a_degenerate_card_draws_nothing() {
        let bar = toolbar_of(0, 1.0);
        assert!(bar.back().is_none());
        assert!(bar.filter().is_none());
        assert_eq!(bar.hit(12, 12), None);
    }

    #[test]
    fn a_hidpi_card_keeps_every_control() {
        let bar = toolbar_of(Spacing::new(13.0, 2.0).card_width(), 2.0);
        let plain = toolbar_of(256, 1.0);
        assert!(bar.back().expect("back").w > plain.back().expect("back").w);
        assert_eq!(all(&bar).len(), 7);
    }
}
