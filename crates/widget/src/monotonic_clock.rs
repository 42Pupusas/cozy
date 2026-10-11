use std::sync::OnceLock;
use std::time::Instant;

pub struct MonotonicClock;

impl MonotonicClock {
    #[must_use]
    pub fn now_ms() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::MonotonicClock;

    #[test]
    fn time_does_not_run_backwards() {
        let first = MonotonicClock::now_ms();
        let second = MonotonicClock::now_ms();
        assert!(second >= first);
    }
}
