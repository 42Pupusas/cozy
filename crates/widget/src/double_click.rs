#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoubleClick {
    last: Option<(u64, (i32, i32))>,
}

impl DoubleClick {
    pub const WINDOW_MS: u64 = 400;
    pub const SLOP: i32 = 4;

    #[must_use]
    pub const fn new() -> Self {
        Self { last: None }
    }

    pub fn register(&mut self, now_ms: u64, at: (i32, i32)) -> bool {
        let doubled = self.last.is_some_and(|(then, prev)| {
            now_ms.saturating_sub(then) <= Self::WINDOW_MS
                && (prev.0 - at.0).abs() <= Self::SLOP
                && (prev.1 - at.1).abs() <= Self::SLOP
        });
        self.last = if doubled { None } else { Some((now_ms, at)) };
        doubled
    }
}

#[cfg(test)]
mod tests {
    use super::DoubleClick;

    #[test]
    fn a_first_click_is_single() {
        assert!(!DoubleClick::new().register(0, (10, 10)));
    }

    #[test]
    fn a_quick_second_click_nearby_is_double() {
        let mut clicks = DoubleClick::new();
        clicks.register(0, (10, 10));
        assert!(clicks.register(DoubleClick::WINDOW_MS, (12, 9)));
    }

    #[test]
    fn a_slow_second_click_is_single() {
        let mut clicks = DoubleClick::new();
        clicks.register(0, (10, 10));
        assert!(!clicks.register(DoubleClick::WINDOW_MS + 1, (10, 10)));
    }

    #[test]
    fn a_distant_second_click_is_single() {
        let mut clicks = DoubleClick::new();
        clicks.register(0, (10, 10));
        assert!(!clicks.register(50, (10 + DoubleClick::SLOP + 1, 10)));
    }

    #[test]
    fn a_third_click_does_not_pair_with_the_second() {
        let mut clicks = DoubleClick::new();
        clicks.register(0, (10, 10));
        assert!(clicks.register(50, (10, 10)));
        assert!(!clicks.register(100, (10, 10)));
    }

    #[test]
    fn a_missed_pair_starts_a_new_one_from_the_later_click() {
        let mut clicks = DoubleClick::new();
        clicks.register(0, (10, 10));
        clicks.register(1_000, (10, 10));
        assert!(clicks.register(1_100, (10, 10)));
    }
}
