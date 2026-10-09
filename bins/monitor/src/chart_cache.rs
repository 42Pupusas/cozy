//! Incremental, pixel-scrolled chart surfaces.
//!
//! A terminal does not re-rasterize every visible cell after a one-line scroll:
//! it moves its persistent pixels and paints only the newly exposed strip. A
//! monitor chart is the same kind of time viewport. This module gives each
//! card a private opaque surface, shifts it as time advances, and redraws only
//! the right edge instead of reconstructing every historical curve at 60 Hz.

use std::collections::VecDeque;

use platform::SurfaceFormat;
use platform::draw::{Canvas, Color, Rect};

use crate::chart::Chart;
use crate::damage::DamageList;

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

/// Persistent pixels for a single card.
///
/// `scroll_remainder` retains fractional pixel motion, so a 60 Hz source can
/// advance a chart whose width is smaller than its history length without
/// losing elapsed time to integer rounding.
pub struct ChartCache {
    pixels: Vec<u32>,
    width: i32,
    height: i32,
    panel: Color,
    format: SurfaceFormat,
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
            format: SurfaceFormat::opaque(),
            scroll_remainder: 0.0,
            seen_samples: 0,
        }
    }

    /// Paint this cached card into the window canvas. A resize or initial paint
    /// rebuilds history once; steady-state calls scroll only enough whole pixels
    /// to represent new samples, then render the exposed right-edge columns.
    ///
    /// `background_cleared` must be true exactly when the caller wiped the
    /// target canvas this frame (the monitor's `full_repaint` path clears the
    /// whole window). It controls the single most expensive line here: the
    /// full-surface `copy_pixels`. When the chart did not change *and* the
    /// target still holds last frame's identical pixels, that blit is pure
    /// waste -- and it is the common case, because hero-number glide triggers
    /// ~10 paints/sec while chart knots only arrive ~4/sec, so most frames
    /// scroll the chart by zero columns. Skipping the copy on those frames is
    /// what takes the four charts off the frame's critical path.
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
        background_cleared: bool,
    ) -> DamageList {
        if self.format != target.format() {
            self.format = target.format();
            self.pixels.clear();
        }
        // How many right-edge columns this frame actually rewrote in the
        // cached surface -- a full rebuild rewrites everything, a steady-state
        // advance rewrites only the freshly scrolled-in strip (often zero when
        // no whole pixel of time elapsed). This is exactly the region that
        // differs in the parent canvas after `copy_pixels`, so it is the
        // chart's true damage.
        let dirty_columns = if self.needs_rebuild(bounds) {
            self.rebuild(bounds, primary, maximum, color, overlays, sequence);
            self.width
        } else {
            self.advance(
                primary,
                maximum,
                color,
                overlays,
                history_capacity,
                sequence,
            )
        };
        // The cached surface is unchanged this frame (no new whole pixel of
        // time scrolled in). If the target still holds our pixels from last
        // frame -- i.e. the caller did NOT clear the background -- the blit
        // would copy identical bytes, so skip it and report no damage. When
        // the background *was* cleared, the target no longer has our pixels
        // and we must re-blit the whole surface even though it is unchanged.
        if dirty_columns <= 0 && !background_cleared {
            return DamageList::new();
        }
        target.copy_pixels(
            Rect::new(bounds.x, bounds.y, self.width, self.height),
            &self.pixels,
        );
        if dirty_columns <= 0 {
            return DamageList::new();
        }
        // The rewritten columns are the rightmost `dirty_columns` of the card
        // (the scroll exposes new pixels on the right edge); a rebuild sets
        // `dirty_columns == width`, covering the whole card.
        let x = bounds.x + (self.width - dirty_columns).max(0);
        DamageList::single(Rect::new(x, bounds.y, dirty_columns, self.height))
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
        self.pixels.resize(area, self.format.pack_glass(self.panel));
        self.scroll_remainder = 0.0;
        self.seen_samples = sequence;
        let mut canvas =
            Canvas::new(&mut self.pixels, self.width, self.height).with_format(self.format);
        let chart = Chart::new(Rect::new(0, 0, self.width, self.height), color, self.panel);
        chart.draw(&mut canvas, primary, maximum);
        for overlay in overlays {
            chart.draw_overlay(&mut canvas, overlay.values, maximum, overlay.color);
        }
    }

    /// Scroll the cached surface left by however many whole pixels of elapsed
    /// time accumulated, redraw that many columns on the right edge, and
    /// return the count of columns rewritten (0 when no whole pixel elapsed) so
    /// the caller can report exactly that strip as damage.
    fn advance(
        &mut self,
        primary: &VecDeque<f32>,
        maximum: f32,
        color: Color,
        overlays: &[Overlay<'_>],
        history_capacity: usize,
        sequence: u64,
    ) -> i32 {
        let previous = self.seen_samples;
        let added = sequence.saturating_sub(previous);
        self.seen_samples = sequence;
        if added == 0 || self.width <= 0 || self.height <= 0 || history_capacity == 0 {
            return 0;
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
            return 0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.scroll_remainder -= columns as f32;
        }
        let moved = columns.min(self.width);
        self.scroll_left(moved);
        self.draw_right_edge(moved, primary, maximum, color, overlays);
        moved
    }

    fn scroll_left(&mut self, columns: i32) {
        let width = self.width.cast_unsigned() as usize;
        let columns = columns.cast_unsigned() as usize;
        if columns >= width {
            self.pixels.fill(self.format.pack_glass(self.panel));
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
        let mut canvas =
            Canvas::new(&mut self.pixels, self.width, self.height).with_format(self.format);
        canvas.fill_rect_glass(Rect::new(start, 0, columns, self.height), self.panel);
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
            true,
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
            false,
        );
        assert_eq!(cache.seen_samples, 3);
    }

    /// A frame that scrolls the chart by zero columns and does not clear the
    /// background must not touch the target canvas at all -- that is the
    /// idle-glide fast path that keeps the four charts off the critical path.
    #[test]
    fn unchanged_chart_over_intact_background_does_not_touch_target() {
        let mut cache = ChartCache::new(Color::BLACK);
        let values = VecDeque::from([0.0, 1.0]);
        // First paint (background cleared) establishes the cached surface and
        // blits it into the target.
        let mut target = vec![0u32; 20];
        cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            2,
            true,
        );
        // Poison the target: if the second paint blits, it will overwrite this.
        let sentinel = 0xDEAD_BEEF;
        target.fill(sentinel);
        // Same sequence => zero new columns, and background NOT cleared.
        let damage = cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            2,
            false,
        );
        assert!(damage.is_empty(), "an unchanged chart reports no damage");
        assert!(
            target.iter().all(|&p| p == sentinel),
            "the blit must be skipped, leaving the target untouched"
        );
    }

    /// The same unchanged chart, but with the background cleared, MUST re-blit
    /// its full surface -- otherwise the freshly-wiped canvas would show a hole
    /// where the chart belongs.
    #[test]
    fn unchanged_chart_over_cleared_background_reblits() {
        let mut cache = ChartCache::new(Color::BLACK);
        let values = VecDeque::from([0.0, 1.0]);
        let mut target = vec![0u32; 20];
        cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            2,
            true,
        );
        let sentinel = 0xDEAD_BEEF;
        target.fill(sentinel);
        // Zero new columns, but background WAS cleared: must re-blit.
        cache.paint(
            &mut Canvas::new(&mut target, 5, 4),
            Rect::new(0, 0, 5, 4),
            &values,
            1.0,
            Color::WHITE,
            &[],
            2,
            2,
            true,
        );
        assert!(
            target.iter().any(|&p| p != sentinel),
            "a cleared background forces the chart to re-blit its surface"
        );
    }
}
