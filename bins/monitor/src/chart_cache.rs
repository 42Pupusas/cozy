//! Incremental, pixel-scrolled chart surfaces.
//!
//! A terminal does not re-rasterize every visible cell after a one-line scroll:
//! it moves its persistent pixels and paints only the newly exposed strip. A
//! monitor chart is the same kind of time viewport. This module gives each
//! card a private opaque surface, shifts it as time advances, and redraws only
//! the right edge instead of reconstructing every historical curve at 60 Hz.

use std::collections::VecDeque;

use platform::draw::{Canvas, Color, Rect};

use crate::chart::Chart;

/// One unfilled comparison series, used by the CPU card's per-core overlays.
pub struct Overlay<'a> {
    values: &'a VecDeque<f32>,
    color: Color,
}

impl<'a> Overlay<'a> {
    #[must_use]
    pub const fn new(values: &'a VecDeque<f32>, color: Color) -> Self {
        Self { values, color }
    }
}

/// Persistent pixels for a single card. `scroll_remainder` retains fractional
/// pixel motion, so a 60 Hz source can advance a chart whose width is smaller
/// than its history length without losing elapsed time to integer rounding.
pub struct ChartCache {
    pixels: Vec<u32>,
    width: i32,
    height: i32,
    panel: Color,
    scroll_remainder: f32,
    seen_samples: u64,
}

impl ChartCache {
    #[must_use]
    pub const fn new(panel: Color) -> Self {
        Self {
            pixels: Vec::new(),
            width: 0,
            height: 0,
            panel,
            scroll_remainder: 0.0,
            seen_samples: 0,
        }
    }

    /// Paint this cached card into the window canvas. A resize or initial paint
    /// rebuilds history once; steady-state calls scroll only enough whole pixels
    /// to represent new samples, then render the exposed right-edge columns.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        target: &mut Canvas,
        bounds: Rect,
        primary: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        overlays: &[Overlay<'_>],
        history_capacity: usize,
        sequence: u64,
    ) {
        if self.needs_rebuild(bounds) {
            self.rebuild(bounds, primary, maximum, color, overlays, sequence);
        } else {
            self.advance(
                primary,
                maximum,
                color,
                overlays,
                history_capacity,
                sequence,
            );
        }
        target.copy_pixels(
            Rect::new(bounds.x, bounds.y, self.width, self.height),
            &self.pixels,
        );
    }

    const fn needs_rebuild(&self, bounds: Rect) -> bool {
        // `seen_samples` is a monotonic sequence number, not a history length.
        // Comparing it with the capped VecDeque length would make every frame
        // rebuild once the sequence exceeded the history capacity, causing a
        // delayed long-session CPU regression. Sequence tracking belongs only
        // to incremental advancement; geometry and allocation determine when
        // a full rasterization is necessary.
        self.width != bounds.w || self.height != bounds.h || self.pixels.is_empty()
    }

    fn rebuild(
        &mut self,
        bounds: Rect,
        primary: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        overlays: &[Overlay<'_>],
        sequence: u64,
    ) {
        self.width = bounds.w.max(0);
        self.height = bounds.h.max(0);
        let area = self.width.cast_unsigned() as usize * self.height.cast_unsigned() as usize;
        self.pixels.resize(area, self.panel.to_xrgb8888());
        self.scroll_remainder = 0.0;
        self.seen_samples = sequence;
        let mut canvas = Canvas::new(&mut self.pixels, self.width, self.height);
        let chart = Chart::new(Rect::new(0, 0, self.width, self.height), color, self.panel);
        chart.draw(&mut canvas, primary, maximum);
        for overlay in overlays {
            chart.draw_overlay(&mut canvas, overlay.values, maximum, overlay.color);
        }
    }

    fn advance(
        &mut self,
        primary: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        overlays: &[Overlay<'_>],
        history_capacity: usize,
        sequence: u64,
    ) {
        let previous = self.seen_samples;
        let added = sequence.saturating_sub(previous);
        self.seen_samples = sequence;
        if added == 0 || self.width <= 0 || self.height <= 0 || history_capacity == 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.scroll_remainder += added.to_string().parse::<f32>().unwrap_or(f32::MAX)
                * self.width as f32
                / history_capacity as f32;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let columns = self.scroll_remainder.floor() as i32;
        if columns <= 0 {
            return;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.scroll_remainder -= columns as f32;
        }
        let moved = columns.min(self.width);
        self.scroll_left(moved);
        self.draw_right_edge(moved, primary, maximum, color, overlays);
    }

    fn scroll_left(&mut self, columns: i32) {
        let width = self.width.cast_unsigned() as usize;
        let columns = columns.cast_unsigned() as usize;
        if columns >= width {
            self.pixels.fill(self.panel.to_xrgb8888());
            return;
        }
        for row in 0..self.height.cast_unsigned() as usize {
            let start = row * width;
            self.pixels
                .copy_within(start + columns..start + width, start);
        }
    }

    fn draw_right_edge(
        &mut self,
        columns: i32,
        primary: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        overlays: &[Overlay<'_>],
    ) {
        let start = (self.width - columns).max(0);
        let mut canvas = Canvas::new(&mut self.pixels, self.width, self.height);
        canvas.fill_rect(Rect::new(start, 0, columns, self.height), self.panel);
        for x in start..self.width {
            Self::draw_primary_column(
                &mut canvas,
                x,
                self.height,
                primary,
                maximum,
                color,
                self.panel,
            );
            for overlay in overlays {
                Self::draw_line_column(
                    &mut canvas,
                    x,
                    self.height,
                    overlay.values,
                    maximum,
                    overlay.color,
                );
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn draw_primary_column(
        canvas: &mut Canvas,
        x: i32,
        height: i32,
        values: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        panel: Color,
    ) {
        let y = Self::latest_y(values, maximum, height);
        let fill = panel.blend_opaque_color(color, 24);
        canvas.vline(y.ceil() as i32, height - 2, x, fill);
        Self::draw_line_column(canvas, x, height, values, maximum, color);
    }

    #[allow(clippy::cast_possible_truncation)]
    fn draw_line_column(
        canvas: &mut Canvas,
        x: i32,
        height: i32,
        values: &VecDeque<f32>,
        maximum: f32,
        color: Color,
    ) {
        let y = Self::latest_y(values, maximum, height);
        // A direct 2px column is intentionally used only for newly revealed
        // pixels. The cached historical body retains the full AA monotone
        // render, while this bounded edge work replaces thousands of old-line
        // pixels with a handful of new ones.
        canvas.vline(y.floor() as i32, y.ceil() as i32 + 1, x, color);
    }

    #[allow(clippy::cast_precision_loss)]
    fn latest_y(values: &VecDeque<f32>, maximum: f32, height: i32) -> f32 {
        let normalized =
            (values.back().copied().unwrap_or(0.0) / maximum.max(0.01)).clamp(0.0, 1.0);
        let top = 2.0;
        let bottom = (height - 3).max(2) as f32;
        (bottom - top).mul_add(-normalized, bottom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_moves_previous_pixels_left() {
        let mut cache = ChartCache::new(Color::BLACK);
        let mut values = VecDeque::from([0.0, 1.0]);
        let mut target = vec![0; 20];
        cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            2,
        );
        values.push_back(0.5);
        cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            3,
        );
        assert_eq!(cache.seen_samples, 3);
    }
}
