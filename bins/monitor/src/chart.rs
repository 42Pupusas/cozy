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
        canvas.fill_rect(self.bounds, self.panel);
        if values.len() < 2 || self.bounds.w < 2 || self.bounds.h < 2 {
            return;
        }
        let curve = self.curve(values, maximum.max(0.01));
        self.draw_area(canvas, &curve);
        self.draw_stroke(canvas, &curve);
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    fn curve(&self, values: &VecDeque<f32>, maximum: f32) -> Vec<f32> {
        let samples: Vec<f32> = values
            .iter()
            .map(|value| (value / maximum).clamp(0.0, 1.0))
            .collect();
        let last = samples.len() - 1;
        let mut curve = Vec::with_capacity(self.bounds.w.cast_unsigned() as usize);
        for column in 0..self.bounds.w {
            let position = column as f32 * last as f32 / (self.bounds.w - 1) as f32;
            let index = position.floor() as usize;
            let fraction = position - index as f32;
            let value = if index >= last {
                samples[last]
            } else {
                Self::monotone_cubic(&samples, index, fraction)
            };
            curve.push(self.value_to_y(value));
        }
        curve
    }

    /// Fritsch–Carlson tangents preserve the direction of adjacent samples.
    /// This is vital for telemetry: a renderer must not draw a temperature or
    /// CPU spike merely because interpolation happened to overshoot.
    #[allow(clippy::suboptimal_flops)]
    fn monotone_cubic(samples: &[f32], index: usize, t: f32) -> f32 {
        let previous_slope = if index == 0 {
            samples[1] - samples[0]
        } else {
            samples[index] - samples[index - 1]
        };
        let slope = samples[index + 1] - samples[index];
        let next_slope = if index + 2 == samples.len() {
            slope
        } else {
            samples[index + 2] - samples[index + 1]
        };
        let left_tangent = Self::tangent(previous_slope, slope);
        let right_tangent = Self::tangent(slope, next_slope);
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * samples[index] + h10 * left_tangent + h01 * samples[index + 1] + h11 * right_tangent)
            .clamp(0.0, 1.0)
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
        let bottom = self.bounds.bottom() - 2;
        for (column, &y) in curve.iter().enumerate() {
            let x = self.bounds.x + i32::try_from(column).unwrap_or(i32::MAX);
            let first = y.ceil() as i32;
            for row in first..=bottom {
                canvas.blend_pixel(x, row, self.ink, 24);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn draw_stroke(&self, canvas: &mut Canvas, curve: &[f32]) {
        for (column, &center) in curve.iter().enumerate() {
            let x = self.bounds.x + i32::try_from(column).unwrap_or(i32::MAX);
            let start = center.floor() as i32 - 2;
            for y in start..=start + 4 {
                // Pixel-center coverage gives a true anti-aliased stroke rather
                // than the old cross-shaped glow stamped on every sample.
                let distance = ((y as f32 + 0.5) - center).abs();
                let coverage = (1.35 - distance).clamp(0.0, 1.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let alpha = (coverage * 255.0).round() as u8;
                if alpha != 0 {
                    canvas.blend_pixel(x, y, self.ink, alpha);
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
        let curve = chart.curve(&values, 1.0);
        assert!(curve.iter().all(|y| *y >= 2.0 && *y <= 17.0));
    }
}
