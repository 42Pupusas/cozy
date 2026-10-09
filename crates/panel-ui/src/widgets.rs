//! The widget set every panel paints its popup with.
//!
//! Labeled rows (icon + title + detail + state badge), a drag slider, and
//! a toggle. Pure layout + paint — no input handling; each panel
//! hit-tests against the rects these return.

use platform::draw::{Canvas, Color, Rect};
use statusbar::TextRenderer;

/// Panel colors — derived from `config::Config` the same way
/// `bins/launcher`'s `Theme` is, but kept here so all three panels match.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub highlight_bg: Color,
    /// The quiet-edge color: slider groove, idle toggle track.
    pub border: Color,
    pub bg_alpha: u8,
}

impl Theme {
    /// Derive the palette from `[panel]` -- every role mirroring the
    /// bar's resolved palette by default (which itself mirrors
    /// `[colors]`), so a panel reads as part of the desktop rather
    /// than a foreign window. The derivation lives in `config`'s
    /// `panel_*` accessors where it is testable without a window.
    #[must_use]
    pub fn from_config(cfg: &config::Config) -> Self {
        let bg: Color = cfg.panel_background().into();
        let fg: Color = cfg.panel_foreground().into();
        let dim: Color = cfg.panel_dim().into();
        let accent: Color = cfg.panel_accent().into();
        let highlight_bg: Color = cfg.panel_highlight().into();
        let border: Color = cfg.panel_border().into();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bg_alpha = (cfg.panel_opacity().clamp(0.0, 1.0) * 255.0).round() as u8;
        Self {
            bg,
            fg,
            dim,
            accent,
            highlight_bg,
            border,
            bg_alpha,
        }
    }
}

/// Geometry constants shared by the widget painters.
pub struct Metrics;

impl Metrics {
    pub const PADDING: i32 = 12;
    pub const ROW_HEIGHT: i32 = 34;
    pub const SLIDER_HEIGHT: i32 = 36;
    pub const SECTION_GAP: i32 = 8;
    pub const TOGGLE_WIDTH: i32 = 34;
}

/// Everything one list row shows, bundled so [`Row::paint`] takes one
/// content value instead of seven positional parts.
#[derive(Default)]
pub struct RowContent<'a> {
    pub icon: Option<&'a str>,
    pub title: &'a str,
    pub detail: Option<&'a str>,
    pub badge: Option<&'a str>,
}

impl<'a> RowContent<'a> {
    /// A bare title-only row.
    #[must_use]
    pub const fn titled(title: &'a str) -> Self {
        Self {
            icon: None,
            title,
            detail: None,
            badge: None,
        }
    }
}

/// A clickable list row. `paint` returns the row's full rect so the
/// owning panel can hit-test clicks against it.
pub struct Row;

impl Row {
    /// Paint one row at `y`, returning its rect.
    pub fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        y: i32,
        highlighted: bool,
        content: &RowContent<'_>,
        width: i32,
    ) -> Rect {
        let h = Metrics::ROW_HEIGHT;
        if highlighted {
            canvas.fill_rect(
                Rect::new(Metrics::PADDING, y, width - 2 * Metrics::PADDING, h),
                theme.highlight_bg,
            );
        }
        let cy = y + h / 2;
        let mut x = Metrics::PADDING + 4;
        if let Some(icon) = content.icon {
            x = text.draw_text_alpha(canvas, icon, x, cy, theme.accent, theme.bg, theme.bg_alpha);
            x += 8;
        }
        text.draw_text_alpha(
            canvas,
            content.title,
            x,
            cy,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        let mut right = width - Metrics::PADDING - 4;
        if let Some(badge) = content.badge {
            let badge_w = text.measure(badge);
            right -= badge_w;
            text.draw_text_right_alpha(
                canvas,
                badge,
                right,
                cy,
                theme.dim,
                theme.bg,
                theme.bg_alpha,
            );
            right -= 12;
        }
        if let Some(detail) = content.detail {
            text.draw_text_right_alpha(
                canvas,
                detail,
                right,
                cy,
                theme.dim,
                theme.bg,
                theme.bg_alpha,
            );
        }
        Rect::new(0, y, width, h)
    }
}

/// A horizontal percentage slider: label + value left, draggable groove
/// right. The panel owns the value; `paint` returns the groove rect (the
/// drag hit area) and draws the current fill.
pub struct Slider;

impl Slider {
    /// Paint the slider at `y`, returning the groove rect.
    pub fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        y: i32,
        label: &str,
        value_pct: u32,
        width: i32,
    ) -> Rect {
        let h = Metrics::SLIDER_HEIGHT;
        let cy = y + h / 2;
        let label_end = text.draw_text_alpha(
            canvas,
            label,
            Metrics::PADDING + 4,
            cy,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        let value_str = format!("{value_pct}%");
        text.draw_text_right_alpha(
            canvas,
            &value_str,
            width - Metrics::PADDING - 4,
            cy,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        let groove_x0 = label_end + 10;
        let groove_x1 = width - Metrics::PADDING - 4 - text.measure(&value_str) - 10;
        let groove_y = cy - 2;
        let groove_h = 4;
        let groove_w = (groove_x1 - groove_x0).max(1);
        canvas.fill_rect(
            Rect::new(groove_x0, groove_y, groove_w, groove_h),
            theme.border,
        );
        let span = groove_w;
        // Percent-to-pixels: `value_pct` is 0..=100 and `span` a window
        // width, both far from i32's limits, so this sign change can't wrap.
        let fill_w = (span * value_pct.cast_signed() / 100).clamp(0, span);
        canvas.fill_rect(
            Rect::new(groove_x0, groove_y, fill_w, groove_h),
            theme.accent,
        );
        Rect::new(groove_x0, groove_y - 8, groove_w, groove_h + 16)
    }

    /// Convert a pointer x inside `groove` to the percentage it selects.
    #[must_use]
    pub fn pct_at(groove: Rect, pointer_x: i32) -> u32 {
        let span = groove.w.max(1);
        let pct = (pointer_x - groove.x) * 100 / span;
        pct.clamp(0, 100).cast_unsigned()
    }
}

/// An on/off pill toggle, painted right-aligned. The panel owns the
/// state; `paint` returns the toggle's hit rect.
pub struct Toggle;

impl Toggle {
    /// Paint the toggle with its left edge at `x`, vertically centered on
    /// `cy`, returning its hit rect.
    pub fn paint(canvas: &mut Canvas, theme: &Theme, x: i32, cy: i32, on: bool) -> Rect {
        let w = Metrics::TOGGLE_WIDTH;
        let h = 20;
        let y0 = cy - h / 2;
        let track = if on { theme.accent } else { theme.border };
        canvas.fill_rect(Rect::new(x, y0, w, h), track);
        let knob_x = if on { x + w - 16 } else { x + 2 };
        let ky = y0 + 2;
        canvas.fill_rect(Rect::new(knob_x, ky, 14, h - 4), theme.bg);
        Rect::new(x, y0, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_pct_maps_the_full_groove_span() {
        let groove = Rect::new(100, 10, 200, 20);
        assert_eq!(Slider::pct_at(groove, 100), 0);
        assert_eq!(Slider::pct_at(groove, 200), 50);
        assert_eq!(Slider::pct_at(groove, 300), 100);
    }

    #[test]
    fn slider_pct_clamps_outside_the_groove() {
        let groove = Rect::new(100, 10, 200, 20);
        assert_eq!(Slider::pct_at(groove, 0), 0);
        assert_eq!(Slider::pct_at(groove, 9999), 100);
    }

    #[test]
    fn row_content_titled_builds_a_bare_row() {
        let c = RowContent::titled("plain");
        assert_eq!(c.title, "plain");
        assert!(c.icon.is_none());
        assert!(c.detail.is_none());
        assert!(c.badge.is_none());
    }
}
