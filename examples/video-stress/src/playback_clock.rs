//! Wall-clock to frame-index mapping for paced playback.

use std::time::{Duration, Instant};

/// What the caller must do at one presentation opportunity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Present `index`. `skipped` counts the frames the clock passed over
    /// because the renderer did not keep up with the nominal rate.
    Present { index: usize, skipped: u64 },
    /// The clock has not reached the next frame yet; present nothing.
    Wait,
}

/// Maps elapsed time onto frame indices at a fixed nominal rate.
///
/// The clock is the arbiter of correctness for this harness. It advances on
/// real time, never on the count of completed presents, so a renderer that
/// falls behind reports skipped frames instead of quietly playing in slow
/// motion.
pub struct PlaybackClock {
    started: Instant,
    frame_interval: Duration,
    next_index: u64,
    presented: u64,
}

impl PlaybackClock {
    /// Start a clock at `fps`, which must be non-zero.
    #[must_use]
    pub fn start(fps: u32) -> Self {
        Self::start_at(Instant::now(), fps)
    }

    /// Start a clock from an explicit origin, which makes pacing testable.
    #[must_use]
    pub fn start_at(origin: Instant, fps: u32) -> Self {
        Self {
            started: origin,
            frame_interval: Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1))),
            next_index: 0,
            presented: 0,
        }
    }

    /// The nominal gap between two frames.
    #[must_use]
    pub const fn frame_interval(&self) -> Duration {
        self.frame_interval
    }

    /// How many frames this clock has handed out.
    #[must_use]
    pub const fn presented(&self) -> u64 {
        self.presented
    }

    /// Decide what to present now.
    pub fn tick(&mut self) -> Tick {
        self.tick_at(Instant::now())
    }

    /// Decide what to present at `now`.
    pub fn tick_at(&mut self, now: Instant) -> Tick {
        let due = self.due_index(now);
        if due < self.next_index {
            return Tick::Wait;
        }
        let skipped = due - self.next_index;
        self.next_index = due + 1;
        self.presented += 1;
        Tick::Present {
            index: usize::try_from(due).unwrap_or(usize::MAX),
            skipped,
        }
    }

    /// How long to sleep before the next frame falls due.
    #[must_use]
    pub fn time_until_next(&self, now: Instant) -> Duration {
        let target = self.started + self.frame_interval * self.next_index_u32();
        target.saturating_duration_since(now)
    }

    /// The frame index the clock owes at `now`.
    fn due_index(&self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.started);
        u64::try_from(elapsed.as_nanos() / self.frame_interval.as_nanos().max(1))
            .unwrap_or(u64::MAX)
    }

    /// `Duration` multiplication takes a `u32`; long clips are clamped rather
    /// than allowed to wrap into a nearby target time.
    fn next_index_u32(&self) -> u32 {
        u32::try_from(self.next_index).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Instant, PlaybackClock, Tick};

    #[test]
    fn presents_frame_zero_immediately() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 30);
        assert_eq!(
            clock.tick_at(origin),
            Tick::Present {
                index: 0,
                skipped: 0
            }
        );
    }

    #[test]
    fn waits_inside_one_frame_interval() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 60);
        clock.tick_at(origin);
        assert_eq!(clock.tick_at(origin + Duration::from_millis(4)), Tick::Wait);
    }

    #[test]
    fn advances_one_frame_per_interval() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 60);
        clock.tick_at(origin);
        assert_eq!(
            clock.tick_at(origin + Duration::from_millis(17)),
            Tick::Present {
                index: 1,
                skipped: 0
            }
        );
    }

    #[test]
    fn a_late_renderer_reports_skipped_frames() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 60);
        clock.tick_at(origin);
        assert_eq!(
            clock.tick_at(origin + Duration::from_millis(100)),
            Tick::Present {
                index: 6,
                skipped: 5
            }
        );
    }

    #[test]
    fn counts_only_the_frames_it_handed_out() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 60);
        clock.tick_at(origin);
        clock.tick_at(origin + Duration::from_millis(1));
        clock.tick_at(origin + Duration::from_millis(200));
        assert_eq!(clock.presented(), 2);
    }

    #[test]
    fn reports_the_wait_until_the_next_frame() {
        let origin = Instant::now();
        let mut clock = PlaybackClock::start_at(origin, 100);
        clock.tick_at(origin);
        assert_eq!(
            clock.time_until_next(origin + Duration::from_millis(4)),
            Duration::from_millis(6)
        );
    }
}
