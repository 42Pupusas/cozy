//! Accounting for one playback run.

use std::time::{Duration, Instant};

/// Frame and latency counters gathered while a clip plays.
///
/// The renderer is the subject of the measurement, so the interesting number
/// is not the achieved rate alone but how much of each frame interval the
/// paint consumed, and how often the clock had to skip ahead.
pub struct PlaybackStats {
    started: Instant,
    presented: u64,
    skipped: u64,
    paint_total: Duration,
    paint_worst: Duration,
    label: String,
}

impl PlaybackStats {
    /// Begin a run described by `label`.
    #[must_use]
    pub fn start(label: &str) -> Self {
        Self {
            started: Instant::now(),
            presented: 0,
            skipped: 0,
            paint_total: Duration::ZERO,
            paint_worst: Duration::ZERO,
            label: label.to_owned(),
        }
    }

    /// Record one presented frame that took `paint` to produce, after the
    /// clock skipped `skipped` frames to reach it.
    pub fn record(&mut self, paint: Duration, skipped: u64) {
        self.presented += 1;
        self.skipped += skipped;
        self.paint_total += paint;
        self.paint_worst = self.paint_worst.max(paint);
    }

    /// Frames presented so far.
    #[must_use]
    pub const fn presented(&self) -> u64 {
        self.presented
    }

    /// Frames the clock passed over because painting ran late.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Achieved frames per second across the whole run.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn achieved_fps(&self) -> f64 {
        let elapsed = self.started.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            return 0.0;
        }
        self.presented as f64 / elapsed
    }

    /// Mean time spent painting one frame.
    #[must_use]
    pub fn mean_paint(&self) -> Duration {
        self.paint_total
            .checked_div(u32::try_from(self.presented).unwrap_or(u32::MAX))
            .unwrap_or(Duration::ZERO)
    }

    /// The slowest single paint of the run.
    #[must_use]
    pub const fn worst_paint(&self) -> Duration {
        self.paint_worst
    }

    /// Share of frames the clock had to skip, from zero to one.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn skip_ratio(&self) -> f64 {
        let owed = self.presented + self.skipped;
        if owed == 0 {
            return 0.0;
        }
        self.skipped as f64 / owed as f64
    }

    /// A single-line summary for a terminal.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}: {} frames in {:.2}s | {:.2} fps | skipped {} ({:.2}%) | paint mean {:.3}ms worst {:.3}ms",
            self.label,
            self.presented,
            self.started.elapsed().as_secs_f64(),
            self.achieved_fps(),
            self.skipped,
            self.skip_ratio() * 100.0,
            self.mean_paint().as_secs_f64() * 1000.0,
            self.worst_paint().as_secs_f64() * 1000.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, PlaybackStats};

    #[test]
    fn starts_empty() {
        let stats = PlaybackStats::start("idle");
        assert_eq!(stats.presented(), 0);
        assert_eq!(stats.skipped(), 0);
        assert!(stats.skip_ratio().abs() < f64::EPSILON);
        assert_eq!(stats.mean_paint(), Duration::ZERO);
    }

    #[test]
    fn averages_paint_time_over_presented_frames() {
        let mut stats = PlaybackStats::start("paint");
        stats.record(Duration::from_millis(2), 0);
        stats.record(Duration::from_millis(4), 0);
        assert_eq!(stats.mean_paint(), Duration::from_millis(3));
        assert_eq!(stats.worst_paint(), Duration::from_millis(4));
    }

    #[test]
    fn skip_ratio_counts_owed_frames() {
        let mut stats = PlaybackStats::start("skips");
        stats.record(Duration::from_millis(1), 1);
        stats.record(Duration::from_millis(1), 0);
        assert_eq!(stats.presented(), 2);
        assert_eq!(stats.skipped(), 1);
        assert!((stats.skip_ratio() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn summary_names_the_run() {
        let mut stats = PlaybackStats::start("wallpaper");
        stats.record(Duration::from_millis(1), 0);
        assert!(stats.summary().starts_with("wallpaper: 1 frames"));
    }
}
