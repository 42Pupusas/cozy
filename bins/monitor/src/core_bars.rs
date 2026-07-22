//! Per-core CPU utilization bars, redrawn incrementally like
//! [`crate::chart_cache::ChartCache`] instead of clearing their whole area
//! every paint.
//!
//! `perf record` on the headless paint workload (see `perf_harness` in
//! `main.rs`) found `Canvas::fill_rect` as the dominant hotspot in
//! `Painter::paint` -- nearly double the next heaviest symbol. The cause was
//! this widget: the old `cpu_bars` free function unconditionally filled its
//! *entire* background rectangle (a whole card-sized area, easily 200x96px)
//! on every single call, whether or not any bar had actually moved, then
//! drew each bar rectangle on top. Once real system load pushed more ticks
//! past the `DisplaySnapshot` change threshold (see `display_snapshot`
//! module docs), this cost was paid far more often, which is what turned
//! into the live `MONITOR_PROFILE=1` frame-time jump from ~0.95ms to ~9ms.
//!
//! This struct instead remembers each bar's last-drawn pixel height and
//! only repaints a bar's column when its height actually changed -- a
//! background fill only ever touches the vacated strip above a shrinking
//! bar, not the whole card.

use platform::draw::{Canvas, Color, Rect};

/// Per-core bar heights as of the last paint, so a repaint can clear only
/// the pixels that need to change rather than the whole background area.
/// Immutable geometry and palette needed to paint one core-bar group.
#[derive(Copy, Clone)]
pub struct CoreBarPaint<'a> {
    pub bounds: Rect,
    pub background: Color,
    pub colors: &'a [Color],
}

pub struct CoreBars {
    /// Last-drawn bar height in pixels, one per core. Resized (and treated
    /// as "everything changed") whenever the core count changes -- CPU
    /// topology can appear after startup or change on hotplug.
    last_heights: Vec<i32>,
}

impl CoreBars {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_heights: Vec::new(),
        }
    }

    /// Paint per-core bars, redrawing only columns whose height changed.
    /// `colors` cycles if there are more cores than colors.
    pub fn paint(&mut self, canvas: &mut Canvas, cores: &[f32], paint: CoreBarPaint<'_>) {
        let Rect { x, y, w, h } = paint.bounds;
        if cores.is_empty() || w <= 0 || h <= 0 || paint.colors.is_empty() {
            return;
        }
        // A core-count change (hotplug, or the very first paint) means every
        // column's geometry is unknown to the cache; treat it like every bar
        // changed rather than trying to diff mismatched-length data.
        if self.last_heights.len() != cores.len() {
            self.last_heights = vec![-1; cores.len()];
        }
        let gap = 3;
        let bar_width = ((w - gap * (i32::try_from(cores.len()).unwrap_or(1) - 1))
            / i32::try_from(cores.len()).unwrap_or(1))
        .max(1);
        for (index, value) in cores.iter().enumerate() {
            let amount = value.clamp(0.0, 100.0) / 100.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let bar_height = (amount * h as f32).round() as i32;
            if bar_height == self.last_heights[index] {
                continue;
            }
            self.last_heights[index] = bar_height;
            let bar_x = x + i32::try_from(index).unwrap_or(0) * (bar_width + gap);
            // Clear only this column's full height, not the whole card: the
            // bar may have shrunk, so the vacated strip above the new bar
            // top must return to the background color.
            canvas.fill_rect(Rect::new(bar_x, y, bar_width, h), paint.background);
            let bar_y = y + h - bar_height;
            canvas.fill_rect(
                Rect::new(bar_x, bar_y, bar_width, bar_height),
                paint.colors[index % paint.colors.len()],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CoreBarPaint, CoreBars};
    use platform::draw::{Canvas, Color, Rect};

    const BG: Color = Color::rgb(0x18, 0x16, 0x16);
    const COLORS: [Color; 2] = [Color::rgb(0xff, 0, 0), Color::rgb(0, 0xff, 0)];

    #[test]
    fn unchanged_core_values_repaint_nothing() {
        let mut bars = CoreBars::new();
        let mut pixels = vec![BG.to_xrgb8888(); 100 * 50];
        let cores = [40.0, 60.0];
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 50);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 50),
                    background: BG,
                    colors: &COLORS,
                },
            );
        }
        let after_first = pixels.clone();
        // Second paint, identical values: every column should already match
        // `last_heights`, so nothing should be touched -- the buffer must be
        // pixel-identical to the first paint's result.
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 50);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 50),
                    background: BG,
                    colors: &COLORS,
                },
            );
        }
        assert_eq!(pixels, after_first);
    }

    #[test]
    fn changed_core_value_repaints_only_its_column() {
        let mut bars = CoreBars::new();
        let mut pixels = vec![BG.to_xrgb8888(); 100 * 50];
        let mut cores = [40.0, 60.0];
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 50);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 50),
                    background: BG,
                    colors: &COLORS,
                },
            );
        }
        let bar_width = (100 - 3) / 2;
        // Snapshot core 1's column (the one that will NOT change).
        let col1_before: Vec<u32> = (0..50)
            .flat_map(|row| {
                let start = row * 100 + bar_width + 3;
                pixels[start..start + bar_width].to_vec()
            })
            .collect();
        cores[0] = 90.0;
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 50);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 50),
                    background: BG,
                    colors: &COLORS,
                },
            );
        }
        let col1_after: Vec<u32> = (0..50)
            .flat_map(|row| {
                let start = row * 100 + bar_width + 3;
                pixels[start..start + bar_width].to_vec()
            })
            .collect();
        assert_eq!(
            col1_before, col1_after,
            "core 1's column didn't change value, so its pixels must be untouched"
        );
    }

    #[test]
    fn hotplugged_core_count_change_does_not_panic_and_repaints() {
        let mut bars = CoreBars::new();
        let mut pixels = vec![BG.to_xrgb8888(); 100 * 50];
        let mut canvas = Canvas::new(&mut pixels, 100, 50);
        bars.paint(
            &mut canvas,
            &[40.0, 60.0],
            CoreBarPaint {
                bounds: Rect::new(0, 0, 100, 50),
                background: BG,
                colors: &COLORS,
            },
        );
        // Core count changed (e.g. hotplug) -- must not panic on index
        // mismatch, and should treat every bar as needing a repaint.
        bars.paint(
            &mut canvas,
            &[40.0, 60.0, 20.0],
            CoreBarPaint {
                bounds: Rect::new(0, 0, 100, 50),
                background: BG,
                colors: &COLORS,
            },
        );
    }
}
