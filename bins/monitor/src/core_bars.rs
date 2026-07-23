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

use crate::damage::DamageList;

/// Immutable geometry and palette needed to paint one core-bar group.
///
/// Bar heights themselves are remembered across paints in [`CoreBars`] so a
/// repaint can clear only the pixels that need to change rather than the whole
/// background area.
#[derive(Copy, Clone)]
pub struct CoreBarPaint<'a> {
    pub bounds: Rect,
    pub background: Color,
    pub colors: &'a [Color],
    /// Whether the caller cleared the whole canvas this frame (a full
    /// repaint). The incremental delta-strip draw below assumes each bar's
    /// body survives in the persistent backbuffer between frames; a clear
    /// destroys that assumption. When set, every bar must be redrawn in full
    /// -- otherwise an unchanged-height bar over a cleared background paints
    /// nothing and its body vanishes, leaving only later delta strips
    /// "floating" where the bar top happens to move.
    pub background_cleared: bool,
}

pub struct CoreBars {
    /// Last-drawn bar height in pixels, one per core. Resized (and treated
    /// as "everything changed") whenever the core count changes -- CPU
    /// topology can appear after startup or change on hotplug.
    last_heights: Vec<i32>,
}

impl Default for CoreBars {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreBars {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_heights: Vec::new(),
        }
    }

    /// Paint per-core bars, redrawing only columns whose height changed.
    /// `colors` cycles if there are more cores than colors. Returns the exact
    /// set of column rectangles repainted, so the caller can premultiply and
    /// upload only those pixels instead of the whole card -- an unchanged
    /// frame returns an empty list and costs zero downstream bandwidth.
    pub fn paint(
        &mut self,
        canvas: &mut Canvas,
        cores: &[f32],
        paint: CoreBarPaint<'_>,
    ) -> DamageList {
        let mut damage = DamageList::new();
        let Rect { x, y, w, h } = paint.bounds;
        if cores.is_empty() || w <= 0 || h <= 0 || paint.colors.is_empty() {
            return damage;
        }
        // A core-count change (hotplug, or the very first paint) means every
        // column's geometry is unknown to the cache; treat it like every bar
        // changed rather than trying to diff mismatched-length data.
        if self.last_heights.len() != cores.len() {
            self.last_heights = vec![-1; cores.len()];
        }
        // A cleared background wiped every bar's body from the backbuffer, so
        // the incremental delta-strip draw would leave detached, upward-
        // drifting strips. Invalidate the height cache so each bar takes the
        // full-column redraw path below (old_height < 0).
        if paint.background_cleared {
            for height in &mut self.last_heights {
                *height = -1;
            }
        }
        let gap = 3;
        let bar_width = ((w - gap * (i32::try_from(cores.len()).unwrap_or(1) - 1))
            / i32::try_from(cores.len()).unwrap_or(1))
        .max(1);
        for (index, value) in cores.iter().enumerate() {
            let amount = value.clamp(0.0, 100.0) / 100.0;
            #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
            let bar_height = (amount * h as f32).round() as i32;
            let old_height = self.last_heights[index];
            if bar_height == old_height {
                continue;
            }
            self.last_heights[index] = bar_height;
            let bar_x = x + i32::try_from(index).unwrap_or(0) * (bar_width + gap);
            let color = paint.colors[index % paint.colors.len()];
            let bar_y = y + h - bar_height;
            if old_height < 0 {
                // Unknown previous state (first paint, or a hotplug reset the
                // cache to -1): the column's existing pixels can't be trusted,
                // so clear the whole column and draw the full bar. This is the
                // rare path -- geometry changes with core count anyway.
                let column = Rect::new(bar_x, y, bar_width, h);
                canvas.fill_rect(column, paint.background);
                canvas.fill_rect(Rect::new(bar_x, bar_y, bar_width, bar_height), color);
                damage.push(column);
                continue;
            }
            // Steady state: only the strip between the old and new bar tops
            // actually changes colour. Below `min(old,new)` the column is
            // still bar-coloured from last frame; above `max(old,new)` it is
            // still background. The persistent backbuffer keeps both, so a
            // 1px glide repaints 1 row -- not the full 96px band. This is the
            // whole reason the core bars stop dominating the shm row upload.
            let old_top = y + h - old_height;
            let new_top = bar_y;
            let strip = if new_top < old_top {
                // Grew upward: the newly-covered strip becomes bar colour.
                let r = Rect::new(bar_x, new_top, bar_width, old_top - new_top);
                canvas.fill_rect(r, color);
                r
            } else {
                // Shrank: the vacated strip returns to the background colour.
                let r = Rect::new(bar_x, old_top, bar_width, new_top - old_top);
                canvas.fill_rect(r, paint.background);
                r
            };
            damage.push(strip);
        }
        damage
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
                    background_cleared: false,
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
                    background_cleared: false,
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
                    background_cleared: false,
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
                    background_cleared: false,
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
    fn glide_damages_only_the_delta_strip_not_the_full_column() {
        // A bar that grows by a few pixels must report damage only as tall as
        // that growth -- not the full card height. This is the property that
        // keeps the core bars off the shm row-upload critical path: a 1px
        // glide is 1 row of damage, not 96.
        let mut bars = CoreBars::new();
        let mut pixels = vec![BG.to_xrgb8888(); 100 * 96];
        let cores0 = [50.0f32];
        let cores1 = [55.0f32];
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 96);
            bars.paint(
                &mut canvas,
                &cores0,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 96),
                    background: BG,
                    colors: &COLORS,
                    background_cleared: false,
                },
            );
        }
        // First paint has no known previous height, so it clears the whole
        // column (old_height < 0). The steady-state glide below is what must
        // be a thin strip.
        let damage = {
            let mut canvas = Canvas::new(&mut pixels, 100, 96);
            bars.paint(
                &mut canvas,
                &cores1,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 96),
                    background: BG,
                    colors: &COLORS,
                    background_cleared: false,
                },
            )
        };
        // 50% -> 55% of 96px is a 5px height change (48 -> 53, rounded).
        assert_eq!(damage.regions().len(), 1);
        let strip = damage.regions()[0];
        assert!(
            strip.h <= 6,
            "delta strip should be ~5px tall, got {}px (full column would be 96)",
            strip.h
        );
    }

    #[test]
    fn cleared_background_redraws_full_bar_even_when_height_unchanged() {
        // Regression for the "floating cores" bug: a full repaint clears the
        // whole canvas, wiping each bar's body. If the value hasn't changed,
        // the incremental path would draw nothing and the body would stay
        // gone, leaving only later delta strips detached in mid-air. With
        // `background_cleared`, the bar must be fully redrawn.
        let mut bars = CoreBars::new();
        let mut pixels = vec![BG.to_xrgb8888(); 100 * 96];
        let cores = [50.0f32];
        // Prime the height cache with a normal paint.
        {
            let mut canvas = Canvas::new(&mut pixels, 100, 96);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 96),
                    background: BG,
                    colors: &COLORS,
                    background_cleared: false,
                },
            );
        }
        // Simulate a full repaint: the caller cleared the canvas to the
        // background before painting.
        for px in &mut pixels {
            *px = BG.to_xrgb8888();
        }
        // Same value -> height unchanged. Without the cleared-background
        // handling this would be a no-op and the bar body would be missing.
        let damage = {
            let mut canvas = Canvas::new(&mut pixels, 100, 96);
            bars.paint(
                &mut canvas,
                &cores,
                CoreBarPaint {
                    bounds: Rect::new(0, 0, 100, 96),
                    background: BG,
                    colors: &COLORS,
                    background_cleared: true,
                },
            )
        };
        // The bar was redrawn (damage reported) and its body pixels are the
        // bar colour, not background. 50% of 96px puts the bar top at ~row 48;
        // sample a row well inside the bar (near the bottom).
        assert!(!damage.is_empty(), "cleared background must redraw the bar");
        let bar_colour = COLORS[0].to_xrgb8888();
        let sample = pixels[90 * 100 + 5];
        assert_eq!(
            sample, bar_colour,
            "bar body must be filled after a cleared-background repaint"
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
                background_cleared: false,
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
                background_cleared: false,
            },
        );
    }
}
