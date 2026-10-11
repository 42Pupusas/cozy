pub struct Deadline;

impl Deadline {
    #[must_use]
    pub const fn soonest(repeat_ms: i32, motion_ms: Option<i32>) -> Option<i32> {
        match (repeat_ms, motion_ms) {
            (repeat, Some(motion)) if repeat < 0 || motion < repeat => Some(motion),
            (repeat, _) if repeat >= 0 => Some(repeat),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Deadline;

    #[test]
    fn nothing_due_means_no_deadline() {
        assert_eq!(Deadline::soonest(-1, None), None);
    }

    #[test]
    fn a_held_key_alone_sets_the_deadline() {
        assert_eq!(Deadline::soonest(30, None), Some(30));
    }

    #[test]
    fn a_glide_alone_sets_the_deadline() {
        assert_eq!(Deadline::soonest(-1, Some(16)), Some(16));
    }

    #[test]
    fn the_sooner_of_the_two_wins() {
        assert_eq!(Deadline::soonest(30, Some(16)), Some(16));
        assert_eq!(Deadline::soonest(10, Some(16)), Some(10));
    }

    #[test]
    fn a_repeat_due_now_is_not_postponed_by_a_glide() {
        assert_eq!(Deadline::soonest(0, Some(16)), Some(0));
    }
}
