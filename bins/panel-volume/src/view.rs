//! Volume panel paint pass.

use panel_ui::{Metrics, Row, RowContent, Slider, Theme};
use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;

use crate::model::{ControlKind, PANEL_WIDTH, VISIBLE_CONTROLS, VolumeModel};

#[derive(Debug, Clone)]
pub enum ControlHit {
    Row { index: usize, rect: Rect },
    Slider { index: usize, rect: Rect },
}

impl ControlHit {
    #[must_use]
    pub const fn rect(&self) -> Rect {
        match self {
            Self::Row { rect, .. } | Self::Slider { rect, .. } => *rect,
        }
    }

    #[must_use]
    pub const fn index(&self) -> usize {
        match self {
            Self::Row { index, .. } | Self::Slider { index, .. } => *index,
        }
    }
}

pub struct VolumeView;

impl VolumeView {
    #[must_use]
    pub fn paint(
        canvas: &mut Canvas,
        text: &mut TextRenderer,
        theme: &Theme,
        model: &VolumeModel,
    ) -> Vec<ControlHit> {
        let controls = model.controls();
        let mut hits = Vec::new();
        let mut y = Metrics::PADDING;
        text.draw_text_alpha(
            canvas,
            "Volume",
            Metrics::PADDING + 4,
            y + 8,
            theme.fg,
            theme.bg,
            theme.bg_alpha,
        );
        y += 28;
        let mut section = "";
        for (index, control) in controls
            .iter()
            .enumerate()
            .skip(model.scroll)
            .take(VISIBLE_CONTROLS)
        {
            if control.section != section {
                section = control.section;
                text.draw_text_alpha(
                    canvas,
                    section,
                    Metrics::PADDING + 4,
                    y + 8,
                    theme.dim,
                    theme.bg,
                    theme.bg_alpha,
                );
                y += 22;
            }
            match &control.kind {
                ControlKind::Device { is_default, .. } => {
                    let badge = if *is_default {
                        Some("default")
                    } else {
                        Some("select")
                    };
                    let rect = Row::paint(
                        canvas,
                        text,
                        theme,
                        y,
                        index == model.selected,
                        &RowContent {
                            icon: Some(if control.section == "Output devices" {
                                "󰓃"
                            } else {
                                "󰍬"
                            }),
                            title: &control.label,
                            detail: None,
                            badge,
                        },
                        PANEL_WIDTH,
                    );
                    hits.push(ControlHit::Row { index, rect });
                    y += Metrics::ROW_HEIGHT;
                }
                ControlKind::Slider { percent, muted, .. } => {
                    if index == model.selected {
                        canvas.fill_rect(
                            Rect::new(
                                Metrics::PADDING,
                                y,
                                PANEL_WIDTH - 2 * Metrics::PADDING,
                                Metrics::SLIDER_HEIGHT,
                            ),
                            theme.highlight_bg,
                        );
                    }
                    let label = if *muted {
                        format!("{}  muted", control.label)
                    } else {
                        control.label.clone()
                    };
                    let rect = Slider::paint(canvas, text, theme, y, &label, *percent, PANEL_WIDTH);
                    hits.push(ControlHit::Slider { index, rect });
                    y += Metrics::SLIDER_HEIGHT;
                }
            }
        }
        hits
    }
}
