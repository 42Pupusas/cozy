//! [`Fade`] -- one value moving between 0 and 1 at a constant rate, eased
//! only when read, so a fade reversed half way turns around from where it is.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fade {
    progress: f32,
    target: f32,
}

impl Fade {
    #[must_use]
    pub const fn rising() -> Self {
        Self {
            progress: 0.0,
            target: 1.0,
        }
    }

    #[must_use]
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
        let delta = f32::from(elapsed_ms) / f32::from(duration_ms.max(1));
        self.progress = if self.target > self.progress {
            (self.progress + delta).min(self.target)
        } else {
            (self.progress - delta).max(self.target)
        };
    }

    #[must_use]
    pub fn is_settled(self) -> bool {
        (self.progress - self.target).abs() < f32::EPSILON
    }

    #[must_use]
    pub fn is_gone(self) -> bool {
        self.progress <= 0.0 && self.target <= 0.0
    }

    /// Smoothstep: slow at both ends.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn level(self) -> u8 {
        let t = self.progress.clamp(0.0, 1.0);
        let eased = t * t * 2.0f32.mul_add(-t, 3.0);
        (eased * 255.0).round() as u8
    }

    /// Cubic ease-out: fast at once, braking into the end.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn braking_level(self) -> u8 {
        let left = 1.0 - self.progress.clamp(0.0, 1.0);
        let eased = (left * left).mul_add(-left, 1.0);
        (eased * 255.0).round() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::Fade;

    #[test]
    fn a_rising_fade_reaches_full_and_settles() {
        let mut fade = Fade::rising();
        assert_eq!(fade.level(), 0);
        fade.step(100, 200);
        assert!(fade.level() > 0 && fade.level() < 255);
        fade.step(100, 200);
        assert_eq!(fade.level(), 255);
        assert!(fade.is_settled());
    }

    #[test]
    fn braking_covers_more_ground_early_than_smoothstep() {
        let mut fade = Fade::rising();
        fade.step(30, 260);
        assert!(fade.braking_level() > fade.level() * 2);
    }

    #[test]
    fn a_reversed_fade_falls_from_where_it_is_and_is_gone_at_zero() {
        let mut fade = Fade::rising();
        fade.step(100, 200);
        let mid = fade.level();
        fade.aim(false);
        fade.step(16, 200);
        assert!(fade.level() < mid && fade.level() > 0);
        fade.step(500, 200);
        assert!(fade.is_gone());
        assert!(!Fade::full().is_gone());
    }
}
