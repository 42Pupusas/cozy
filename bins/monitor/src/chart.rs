//! Small, self-contained chart renderer for monitor time series.
//!
//! The monitor samples once per second. A monotone cubic interpolation makes
//! those sparse readings visually continuous without Catmull–Rom's tendency to
//! overshoot and manufacture peaks that never occurred.

use std::collections::VecDeque;

use platform::draw::{Canvas, Color, Rect};

pub struct Chart {
    bounds: Rect,
    ink: Color,
    panel: Color,
}

impl Chart {
    #[must_use]
    pub const fn new(bounds: Rect, ink: Color, panel: Color) -> Self {
        Self { bounds, ink, panel }
    }

    pub fn draw(&self, canvas: &mut Canvas, values: &VecDeque<f32>, maximum: f32) {
        canvas.fill_rect_glass(self.bounds, self.panel);
        if let Some(curve) = self.curve(values, maximum) {
            self.draw_area(canvas, &curve);
            self.draw_stroke(canvas, &curve, 1.35);
        }
    }

    /// Draw another time series into an already-painted chart. This lets the
    /// aggregate CPU series retain its filled emphasis while individual cores
    /// remain readable as lightweight, unfilled comparison lines.
    pub fn draw_overlay(
        &self,
        canvas: &mut Canvas,
        values: &VecDeque<f32>,
        maximum: f32,
        color: Color,
    ) {
        if let Some(curve) = self.curve(values, maximum) {
            Self::draw_stroke_for(canvas, &curve, self.bounds.x, self.panel, color, 0.85);
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn curve(&self, values: &VecDeque<f32>, maximum: f32) -> Option<Vec<f32>> {
        if values.len() < 2 || self.bounds.w < 2 || self.bounds.h < 2 {
            return None;
        }
        let last = values.len() - 1;
        let mut curve = Vec::with_capacity(self.bounds.w.cast_unsigned() as usize);
        for column in 0..self.bounds.w {
            let position = column as f32 * last as f32 / (self.bounds.w - 1) as f32;
            let index = position.floor() as usize;
            let fraction = position - index as f32;
            let value = if index >= last {
                Self::normalized(values, last, maximum)
            } else {
                Self::monotone_cubic(values, maximum, index, fraction)
            };
            curve.push(self.value_to_y(value));
        }
        Some(curve)
    }

    /// Fritsch–Carlson tangents preserve the direction of adjacent samples.
    /// This is vital for telemetry: a renderer must not draw a temperature or
    /// CPU spike merely because interpolation happened to overshoot.
    #[allow(clippy::suboptimal_flops)]
    fn monotone_cubic(values: &VecDeque<f32>, maximum: f32, index: usize, t: f32) -> f32 {
        let value = |sample| Self::normalized(values, sample, maximum);
        let previous_slope = if index == 0 {
            value(1) - value(0)
        } else {
            value(index) - value(index - 1)
        };
        let slope = value(index + 1) - value(index);
        let next_slope = if index + 2 == values.len() {
            slope
        } else {
            value(index + 2) - value(index + 1)
        };
        let left_tangent = Self::tangent(previous_slope, slope);
        let right_tangent = Self::tangent(slope, next_slope);
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * value(index) + h10 * left_tangent + h01 * value(index + 1) + h11 * right_tangent)
            .clamp(0.0, 1.0)
    }

    fn normalized(values: &VecDeque<f32>, index: usize, maximum: f32) -> f32 {
        (values[index] / maximum.max(0.01)).clamp(0.0, 1.0)
    }

    fn tangent(left: f32, right: f32) -> f32 {
        if left * right <= 0.0 {
            0.0
        } else {
            2.0 * left * right / (left + right)
        }
    }

    #[allow(clippy::cast_precision_loss, clippy::suboptimal_flops)]
    fn value_to_y(&self, value: f32) -> f32 {
        let top = (self.bounds.y + 2) as f32;
        let bottom = (self.bounds.bottom() - 3) as f32;
        bottom - (bottom - top) * value
    }

    #[allow(clippy::cast_possible_truncation)]
    fn draw_area(&self, canvas: &mut Canvas, curve: &[f32]) {
        // `draw` has just filled this whole rectangle with `self.panel`, so
        // every fill pixel has the identical background. Preblend once, then
        // use Canvas' direct vertical span writer. The old per-pixel alpha path
        // unpacked/reblended thousands of already-known opaque panel pixels on
        // every 60 Hz frame and dominated monitor CPU time.
        let fill = self.panel.blend_opaque_color(self.ink, 24);
        let bottom = self.bounds.bottom() - 2;
        for (column, &y) in curve.iter().enumerate() {
            let x = self.bounds.x + i32::try_from(column).unwrap_or(i32::MAX);
            canvas.vline(y.ceil() as i32, bottom, x, fill);
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn draw_stroke(&self, canvas: &mut Canvas, curve: &[f32], width: f32) {
        Self::draw_stroke_for(canvas, curve, self.bounds.x, self.panel, self.ink, width);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn draw_stroke_for(
        canvas: &mut Canvas,
        curve: &[f32],
        x_origin: i32,
        background: Color,
        color: Color,
        width: f32,
    ) {
        // Every chart has a known opaque panel behind its line. Build this tiny
        // palette once per stroke, then write the resolved AA colour directly.
        // The former `blend_pixel` path reread/unpacked/reblended the same
        // panel colour for every covered pixel; that was the dominant 60 Hz
        // cost in both the CPU overlays and the other charts.
        let palette: [Color; 256] = std::array::from_fn(|alpha| {
            background.blend_opaque_color(color, u8::try_from(alpha).unwrap_or(u8::MAX))
        });
        for (column, &center) in curve.iter().enumerate() {
            let x = x_origin + i32::try_from(column).unwrap_or(i32::MAX);
            let start = center.floor() as i32 - 2;
            for y in start..=start + 4 {
                // Pixel-center coverage gives a true anti-aliased stroke rather
                // than the old cross-shaped glow stamped on every sample.
                let distance = ((y as f32 + 0.5) - center).abs();
                let opacity = (width - distance).clamp(0.0, 1.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let alpha = (opacity * 255.0).round() as usize;
                if alpha != 0 {
                    canvas.set_pixel(x, y, palette[alpha]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotone_curve_stays_in_chart_bounds() {
        let chart = Chart::new(Rect::new(0, 0, 40, 20), Color::WHITE, Color::BLACK);
        let values = VecDeque::from([0.0, 1.0, 0.2, 0.8]);
        let curve = chart.curve(&values, 1.0).expect("enough samples for curve");
        assert!(curve.iter().all(|y| *y >= 2.0 && *y <= 17.0));
    }
}
