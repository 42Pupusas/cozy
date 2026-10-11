//! The Bluetooth panel's paint pass: header row (adapter state + power
//! toggle), device rows, status line. Pure drawing.

use panel_ui::{Metrics, Row, RowContent, Theme, Toggle};
use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;

use crate::model::{BluetoothModel, PANEL_WIDTH, VISIBLE_ROWS};

/// Marker type grouping the panel's paint pass.
pub struct BluetoothView;

impl BluetoothView {
    /// Paint the whole panel; returns `(row_rects, toggle_rect)` for the
    /// main loop's click dispatch. Row 0 is always the power toggle row
    /// when an adapter exists.
    pub fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &BluetoothModel,
    ) -> (Vec<Rect>, Option<Rect>) {
        let mut rects = Vec::new();
        let mut y = Metrics::PADDING;

        let Some(adapter) = &model.adapter else {
            text.draw_text_alpha(
                canvas,
                "Bluetooth  ·  no adapter",
                Metrics::PADDING + 4,
                y + 8,
                theme.fg,
                theme.bg,
                theme.bg_alpha,
            );
            return (rects, None);
        };
        let state = if adapter.powered {
            if adapter.discovering {
                "discovering…"
            } else {
                "on"
            }
        } else {
            "off"
        };
        let header = format!("Bluetooth  ·  {state}");
        let header_w = text.draw_text_alpha(
            canvas,
            &header,
            Metrics::PADDING + 4,
            y + 8,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        let toggle_x = PANEL_WIDTH - Metrics::PADDING - Metrics::TOGGLE_WIDTH;
        let _ = header_w;
        let toggle_rect = Toggle::paint(canvas, theme, toggle_x, y + 12, adapter.powered);
        y += 32;

        let window = model
            .devices
            .iter()
            .enumerate()
            .skip(model.scroll)
            .take(VISIBLE_ROWS);
        for (idx, device) in window {
            let highlighted = idx == model.selected;
            let icon = icon_for(&device.icon);
            let badge = if device.connected {
                Some("connected")
            } else if device.paired {
                Some("paired")
            } else {
                Some("new")
            };
            let detail = device.address.as_str();
            let title = if device.name.is_empty() {
                device.address.clone()
            } else {
                device.name.clone()
            };
            let rect = Row::paint(
                canvas,
                text,
                theme,
                y,
                highlighted,
                &RowContent {
                    icon: Some(icon),
                    title: &title,
                    detail: Some(detail),
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
        (rects, Some(toggle_rect))
    }
}

/// Map bluez's device-class icon names to Nerd Font glyphs.
fn icon_for(icon: &str) -> &'static str {
    match icon {
        "audio-headsets" | "audio-headphones" => "󰋋",
        "input-mouse" => "󰍽",
        "input-keyboard" => "󰌌",
        "input-tablet" => "󰐶",
        "phone" => "󰄛",
        "camera-video" => "󰕿",
        "audio-card" => "󰓃",
        "input-gaming" => "󰊗",
        "printer" => "󰐪",
        _ => "󰂯",
    }
}
