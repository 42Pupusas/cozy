//! [`Fades`] -- values that glide toward a target instead of jumping to it.
//!
//! Time is an input, never read here, so a fade is the same function of the
//! timestamps it is given in a test and on screen.

/// How long a fade takes to cross its whole range.
const DURATION_MS: u16 = 160;

/// The longest span of time one step may cover. A frame that stalls (a hidden
/// window, a slow present) resumes the glide instead of skipping to its end.
const MAX_STEP_MS: u16 = 64;

/// One value moving between 0 and 1.
///
/// Progress moves at a constant rate and is eased only when read, so a fade
/// reversed half way turns around from where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    progress: f32,
    target: f32,
}

impl Fade {
    pub const fn rising() -> Self {
        Self {
            progress: 0.0,
            target: 1.0,
        }
    }

    pub const fn full() -> Self {
        Self {
            progress: 1.0,
            target: 1.0,
        }
    }

    pub const fn aim(&mut self, up: bool) {
        self.target = if up { 1.0 } else { 0.0 };
    }

    pub fn step(&mut self, elapsed_ms: u16, duration_ms: u16) {
        let delta = f32::from(elapsed_ms) / f32::from(duration_ms);
        self.progress = if self.target > self.progress {
            (self.progress + delta).min(self.target)
        } else {
            (self.progress - delta).max(self.target)
        };
    }

    pub fn is_settled(self) -> bool {
        (self.progress - self.target).abs() < f32::EPSILON
    }

    pub fn is_gone(self) -> bool {
        self.progress <= 0.0 && self.target <= 0.0
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn level(self) -> u8 {
        let t = self.progress.clamp(0.0, 1.0);
        let eased = t * t * 2.0f32.mul_add(-t, 3.0);
        (eased * 255.0).round() as u8
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn braking_level(self) -> u8 {
        let left = 1.0 - self.progress.clamp(0.0, 1.0);
        let eased = (left * left).mul_add(-left, 1.0);
        (eased * 255.0).round() as u8
    }
}

/// A set of fades keyed by what they belong to.
///
/// A key that is wanted rises, a key that is not falls and is forgotten once
/// it reaches zero, so the set only ever holds what is visible or moving.
#[derive(Clone, Debug, Default)]
pub struct Fades<K> {
    entries: Vec<(K, Fade)>,
    clock: Option<u64>,
    duration_ms: u16,
}

impl<K: Copy + PartialEq> Fades<K> {
    #[must_use]
    pub const fn new() -> Self {
        Self::lasting(DURATION_MS)
    }

    /// Fades that take `duration_ms` to cross their whole range.
    #[must_use]
    pub const fn lasting(duration_ms: u16) -> Self {
        Self {
            entries: Vec::new(),
            clock: None,
            duration_ms,
        }
    }

    /// Make `wanted` the keys that rise and every other key fall.
    pub fn aim(&mut self, wanted: &[K], now_ms: u64) {
        for (key, fade) in &mut self.entries {
            fade.target = if wanted.contains(key) { 1.0 } else { 0.0 };
        }
        for key in wanted {
            if !self.entries.iter().any(|(held, _)| held == key) {
                self.entries.push((*key, Fade::rising()));
            }
        }
        if self.clock.is_none() && !self.is_settled() {
            self.clock = Some(now_ms);
        }
    }

    /// Move every fade on to `now_ms`.
    pub fn advance(&mut self, now_ms: u64) {
        let Some(then) = self.clock else {
            return;
        };
        let elapsed = now_ms.saturating_sub(then).min(u64::from(MAX_STEP_MS));
        let elapsed = u16::try_from(elapsed).unwrap_or(MAX_STEP_MS);
        self.clock = Some(now_ms.max(then));
        for (_, fade) in &mut self.entries {
            fade.step(elapsed, self.duration_ms);
        }
        self.entries.retain(|(_, fade)| !fade.is_gone());
        if self.is_settled() {
            self.clock = None;
        }
    }

    /// How far `key` has risen, eased, as 0 to 255.
    #[must_use]
    pub fn level(&self, key: &K) -> u8 {
        self.entries
            .iter()
            .find(|(held, _)| held == key)
            .map_or(0, |(_, fade)| fade.level())
    }

    /// Whether any fade is still on its way, so frames are still owed.
    #[must_use]
    pub fn is_animating(&self) -> bool {
        !self.is_settled()
    }

    fn is_settled(&self) -> bool {
        self.entries.iter().all(|(_, fade)| fade.is_settled())
    }
}

#[cfg(test)]
mod tests {
    use super::Fades;

    const FRAME_MS: u64 = 16;

    trait Frames {
        fn run(&mut self, from_ms: u64, frames: u64);
    }

    impl Frames for Fades<u8> {
        fn run(&mut self, from_ms: u64, frames: u64) {
            for frame in 1..=frames {
                self.advance(from_ms + frame * FRAME_MS);
            }
        }
    }

    #[test]
    fn nothing_is_lit_or_moving_at_first() {
        let fades = Fades::<u8>::new();
        assert_eq!(fades.level(&1), 0);
        assert!(!fades.is_animating());
    }

    #[test]
    fn a_wanted_key_rises_over_the_duration_and_settles() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 1000);
        assert!(fades.is_animating());
        assert_eq!(fades.level(&1), 0, "the first frame has not moved yet");
        fades.run(1000, 5);
        let half = fades.level(&1);
        assert!(half > 0 && half < 255, "mid-way is neither end: {half}");
        fades.run(1080, 10);
        assert_eq!(fades.level(&1), 255);
        assert!(!fades.is_animating());
    }

    #[test]
    fn the_rise_is_monotonic() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        let mut last = 0;
        for step in 1..=12 {
            fades.advance(step * FRAME_MS);
            let level = fades.level(&1);
            assert!(level >= last, "level fell from {last} to {level}");
            last = level;
        }
    }

    #[test]
    fn an_unwanted_key_falls_and_is_forgotten() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.run(0, 20);
        assert_eq!(fades.level(&1), 255);
        fades.aim(&[], 400);
        fades.run(400, 20);
        assert_eq!(fades.level(&1), 0);
        assert!(!fades.is_animating());
        assert!(fades.entries.is_empty(), "a dark fade is dropped");
    }

    #[test]
    fn reversing_midway_turns_around_from_where_it_is() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.advance(16);
        fades.advance(48);
        fades.advance(80);
        let before = fades.level(&1);
        fades.aim(&[], 80);
        fades.advance(96);
        let after = fades.level(&1);
        assert!(after < before, "it falls from {before}, not from the top");
        assert!(after > 0, "and does not drop to zero at once");
    }

    #[test]
    fn two_keys_move_independently() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.advance(64);
        fades.aim(&[1, 2], 64);
        fades.advance(96);
        assert!(fades.level(&1) > fades.level(&2));
    }

    #[test]
    fn a_stalled_frame_resumes_instead_of_skipping_to_the_end() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.advance(10_000);
        assert!(
            fades.level(&1) < 255,
            "one stalled frame must not finish the fade"
        );
        assert!(fades.is_animating());
    }

    #[test]
    fn a_longer_fade_is_still_part_way_when_a_shorter_one_has_landed() {
        let mut quick = Fades::new();
        let mut slow = Fades::lasting(400);
        quick.aim(&[1u8], 0);
        slow.aim(&[1u8], 0);
        quick.run(0, 12);
        slow.run(0, 12);
        assert_eq!(quick.level(&1), 255);
        assert!(slow.level(&1) < 255);
    }

    #[test]
    fn staying_wanted_after_settling_costs_no_frames() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.run(0, 20);
        fades.aim(&[1], 400);
        assert!(!fades.is_animating());
    }

    #[test]
    fn a_long_idle_gap_does_not_count_as_elapsed_time() {
        let mut fades = Fades::new();
        fades.aim(&[1u8], 0);
        fades.run(0, 20);
        fades.aim(&[], 60_000);
        assert_eq!(fades.level(&1), 255, "no frame has run yet");
        fades.advance(60_016);
        assert!(fades.level(&1) > 200, "it fell one frame's worth, not all");
    }
}
