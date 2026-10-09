//! [`Theme`] -- every colour and measurement the canvas paints with,
//! resolved once from `[canvas]`/`[colors]` at startup.
//!
//! Same shape as `bins/files`' theme: gathered so a stray literal cannot
//! drift from the user's palette.

use platform::draw::Color;

/// The canvas's resolved palette and metrics.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub background: Color,
    pub grid: Color,
    pub text: Color,
    pub surface: Color,
    pub border: Color,
    pub selection: Color,
    /// The text caret while a note is being edited.
    pub caret: Color,
    /// Caret width in physical pixels.
    pub caret_width: i32,
    /// Shape stroke width in physical pixels (scaled at construction).
    pub stroke_width: i32,
    /// Snap pitch in world units.
    pub snap: i32,
    /// Grid-dot pitch in world units (what the dot lattice is drawn at).
    pub grid_pitch: i32,
    /// Corner radius of note cards.
    pub note_radius: i32,
    /// Length of an arrow's barbs in physical pixels.
    pub arrow_head: i32,
}

impl Theme {
    /// Arrow barb length at scale 1, in physical pixels.
    const ARROW_HEAD: i32 = 12;

    /// Caret width at scale 1, in physical pixels. Two, like the terminal's
    /// beam: one hairline is easy to lose against text.
    const CARET_WIDTH: i32 = 2;

    /// Resolve from the loaded config, at output `scale` (a `HiDPI` scale
    /// multiplies the pixel metrics).
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    pub fn from_config(cfg: &config::Config, scale: f32) -> Self {
        // Pixel metrics are window-scale (a few thousand at most), so
        // rounding through f32 cannot leave the i32 range.
        let stroke = (cfg.canvas_stroke_width() as f32 * scale).round() as i32;
        let snap = (cfg.canvas_grid_size() as f32 * scale).round() as i32;
        let grid_pitch = (cfg.canvas_grid_size().max(1) as f32 * scale).round() as i32;
        Self {
            background: cfg.canvas_background().into(),
            grid: cfg.canvas_grid().into(),
            text: cfg.canvas_text().into(),
            surface: cfg.canvas_surface().into(),
            border: cfg.canvas_border().into(),
            selection: cfg.canvas_selection().into(),
            caret: cfg.canvas_caret().into(),
            caret_width: (Self::CARET_WIDTH as f32 * scale).round() as i32,
            stroke_width: stroke,
            snap,
            grid_pitch,
            note_radius: 6,
            // Scaled, unlike the literal it replaced: an arrowhead fixed at
            // 10 physical pixels shrinks to half its intended size on a
            // HiDPI output.
            arrow_head: (Self::ARROW_HEAD as f32 * scale).round() as i32,
        }
    }
}
