//! [`CardSurface`] -- the pinned card's background, filled from the left in
//! proportion to how full the disk is.
//!
//! One owner for two questions: where the used-space tone ends, and which
//! colours a control painted at a given position must assume is behind it.

use platform::draw::{Canvas, Color, Rect};

use crate::storage::Usage;
use tiles::theme::Theme;

/// The colours that sit behind a control, for one tone of the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backdrop {
    pub surface: Color,
    pub hover: Color,
    pub active: Color,
    pub input: Color,
}

impl Backdrop {
    /// The card's surface where the disk is unused.
    #[must_use]
    pub const fn plain(theme: &Theme) -> Self {
        Self {
            surface: theme.sidebar_bg,
            hover: theme.hover,
            active: theme.sidebar_active,
            input: theme.filter_bg,
        }
    }

    /// The card's surface where the disk is used.
    #[must_use]
    pub const fn filled(theme: &Theme) -> Self {
        Self {
            surface: theme.card_fill,
            hover: theme.card_fill_hover,
            active: theme.card_fill_active,
            input: theme.card_fill_input,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CardSurface {
    bounds: Rect,
    edge: i32,
}

impl CardSurface {
    /// The surface of a card at `bounds`, filled to `usage`. Without usage
    /// figures nothing is filled.
    #[must_use]
    pub fn new(bounds: Rect, usage: Option<Usage>) -> Self {
        let filled = usage.map_or(0, |usage| usage.fill_extent(bounds.w));
        Self {
            bounds,
            edge: bounds.x + filled,
        }
    }

    /// The first column of the plain tone; the card's left edge when the
    /// disk is full and its right edge when nothing is filled.
    #[must_use]
    #[cfg(test)]
    pub const fn edge(&self) -> i32 {
        self.edge
    }

    /// The colours behind a control occupying `band`, judged at its middle
    /// column so a control never straddles two tones.
    #[must_use]
    pub const fn backdrop_for(&self, band: Rect, theme: &Theme) -> Backdrop {
        if band.x + band.w / 2 < self.edge {
            Backdrop::filled(theme)
        } else {
            Backdrop::plain(theme)
        }
    }

    /// Paint the glass surface in its two tones, then the outline.
    pub fn paint(&self, canvas: &mut Canvas<'_>, theme: &Theme) {
        let radius = theme.spacing.radius() * 2;
        let (left, right) = (self.bounds.x, self.bounds.right());
        canvas.fill_round_rect_glass_cols(self.bounds, theme.card_fill, radius, left, self.edge);
        canvas.fill_round_rect_glass_cols(self.bounds, theme.sidebar_bg, radius, self.edge, right);
        canvas.draw_round_rect(self.bounds, theme.sidebar_border, radius);
    }
}

#[cfg(test)]
mod tests {
    use super::CardSurface;
    use crate::storage::Usage;
    use tiles::theme::Theme;
    use platform::draw::{Canvas, Color, Rect};

    const BOUNDS: Rect = Rect::new(10, 10, 100, 60);

    fn rgb(pixel: u32) -> u32 {
        pixel & 0x00ff_ffff
    }

    fn surface_pixel(surface: &CardSurface, x: i32, y: i32) -> u32 {
        let mut buf = vec![0u32; 120 * 80];
        let mut canvas = Canvas::new(&mut buf, 120, 80);
        surface.paint(&mut canvas, &Theme::default());
        rgb(buf[usize::try_from(y * 120 + x).expect("inside the buffer")])
    }

    #[test]
    fn a_thirty_percent_disk_fills_the_left_thirty_percent() {
        let surface = CardSurface::new(BOUNDS, Some(Usage::new(1000, 700)));
        assert_eq!(surface.edge(), BOUNDS.x + 30);
    }

    #[test]
    fn an_unknown_or_empty_disk_fills_nothing() {
        assert_eq!(CardSurface::new(BOUNDS, None).edge(), BOUNDS.x);
        let empty = Usage::new(1000, 1000);
        assert_eq!(CardSurface::new(BOUNDS, Some(empty)).edge(), BOUNDS.x);
    }

    #[test]
    fn a_full_disk_fills_the_whole_card() {
        let surface = CardSurface::new(BOUNDS, Some(Usage::new(1000, 0)));
        assert_eq!(surface.edge(), BOUNDS.right());
    }

    #[test]
    fn the_two_tones_meet_at_the_edge() {
        let theme = Theme::default();
        let surface = CardSurface::new(BOUNDS, Some(Usage::new(1000, 500)));
        let y = BOUNDS.y + BOUNDS.h / 2;
        assert_eq!(
            surface_pixel(&surface, surface.edge() - 1, y),
            rgb(theme.card_fill.to_xrgb8888())
        );
        assert_eq!(
            surface_pixel(&surface, surface.edge(), y),
            rgb(theme.sidebar_bg.to_xrgb8888())
        );
    }

    #[test]
    fn the_filled_tone_differs_from_the_plain_one() {
        let theme = Theme::default();
        assert_ne!(theme.card_fill, theme.sidebar_bg);
        assert_ne!(theme.card_fill_hover, theme.card_fill);
        assert_ne!(theme.card_fill_active, theme.card_fill_hover);
        assert_ne!(theme.card_fill_input, theme.card_fill);
    }

    #[test]
    fn a_control_is_judged_by_its_middle() {
        let theme = Theme::default();
        let surface = CardSurface::new(BOUNDS, Some(Usage::new(1000, 500)));
        let edge = surface.edge();
        let mostly_filled = Rect::new(edge - 8, BOUNDS.y, 12, 12);
        let mostly_plain = Rect::new(edge - 4, BOUNDS.y, 12, 12);
        assert_eq!(
            surface.backdrop_for(mostly_filled, &theme).surface,
            theme.card_fill
        );
        assert_eq!(
            surface.backdrop_for(mostly_plain, &theme).surface,
            theme.sidebar_bg
        );
    }

    #[test]
    fn the_corner_outside_the_arc_stays_untouched() {
        let surface = CardSurface::new(BOUNDS, Some(Usage::new(1000, 0)));
        assert_eq!(
            surface_pixel(&surface, BOUNDS.x, BOUNDS.bottom() - 1),
            rgb(Color::rgb(0, 0, 0).to_xrgb8888())
        );
    }
}
