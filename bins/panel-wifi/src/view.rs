//! The Wi-Fi panel's paint pass: header (device state, scan spinner),
//! the visible row window, password entry, and status line. Pure
//! drawing — every interaction decision stays in the model.

use panel_ui::{Row, RowContent, Theme};
use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;

use crate::model::{PANEL_WIDTH, RowKind, VISIBLE_ROWS, WifiModel};
use panel_ui::Metrics;

/// Signal-strength glyphs, weakest to strongest (Nerd Font).
const BARS: [&str; 4] = ["󰤟", "󰤢", "󰤥", "󰤨"];

/// Marker type grouping the panel's paint pass.
pub struct WifiView;

impl WifiView {
    /// Paint the whole panel; returns the per-row hit rects (parallel to
    /// the model's visible window), for the main loop's click dispatch.
    pub fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &WifiModel,
    ) -> Vec<Rect> {
        let mut rects = Vec::new();
        let mut y = Metrics::PADDING;

        // Header: device interface or its absence.
        let header = model.snapshot.device.as_ref().map_or_else(
            || "Wi-Fi  ·  no device".into(),
            |dev| format!("Wi-Fi  ·  {}", dev.interface),
        );
        text.draw_text_alpha(
            canvas,
            &header,
            Metrics::PADDING + 4,
            y + 8,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        y += 32;

        if model.pending_ap.is_some() {
            return Self::paint_password(canvas, text, theme, model, y);
        }

        let rows = model.visible_rows();
        let window = rows
            .iter()
            .enumerate()
            .skip(model.scroll)
            .take(VISIBLE_ROWS);
        for (idx, row) in window {
            let highlighted = idx == model.selected;
            let icon = match row.kind {
                RowKind::Network => strength_glyph(row.strength),
                RowKind::Disconnect => "󰤭",
            };
            let badge = if row.connected {
                Some("connected")
            } else if row.saved {
                Some("saved")
            } else {
                None
            };
            let lock = if row.secured { "󰌏" } else { "" };
            let detail = if row.kind == RowKind::Network && !lock.is_empty() {
                Some(lock)
            } else {
                None
            };
            let rect = Row::paint(
                canvas,
                text,
                theme,
                y,
                highlighted,
                &RowContent {
                    icon: Some(icon),
                    title: &row.title,
                    detail,
                    badge,
                },
                PANEL_WIDTH,
            );
            rects.push(rect);
            y += Metrics::ROW_HEIGHT;
        }

        if !model.status.is_empty() {
            text.draw_text_alpha(
                canvas,
                &model.status,
                Metrics::PADDING + 4,
                y + 6,
                theme.dim,
                theme.bg,
                theme.bg_alpha,
            );
        }
        rects
    }

    /// The password entry sub-view: prompt, masked input, hint line.
    fn paint_password(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &WifiModel,
        mut y: i32,
    ) -> Vec<Rect> {
        let ap = model.pending_ap.as_ref().map_or("", |ap| ap.ssid.as_str());
        let prompt = format!("Password for {ap}");
        text.draw_text_alpha(
            canvas,
            &prompt,
            Metrics::PADDING + 4,
            y,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        y += 30;
        let masked = "•".repeat(model.password.chars().count());
        text.draw_text_alpha(
            canvas,
            &masked,
            Metrics::PADDING + 4,
            y,
            theme.accent,
            theme.bg,
            theme.bg_alpha,
        );
        y += 30;
        text.draw_text_alpha(
            canvas,
            "Enter to connect · Esc to cancel",
            Metrics::PADDING + 4,
            y,
            theme.dim,
            theme.bg,
            theme.bg_alpha,
        );
        Vec::new()
    }
}

/// Map a 0-100 strength to one of the four bar glyphs.
const fn strength_glyph(strength: u8) -> &'static str {
    match strength {
        0..=25 => BARS[0],
        26..=50 => BARS[1],
        51..=75 => BARS[2],
        _ => BARS[3],
    }
}
