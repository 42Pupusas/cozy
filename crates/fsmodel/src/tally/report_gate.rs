/// Decides when a growing total is worth reporting: the first bytes found,
/// then each time the total has grown by an eighth since the last report.
/// A walk therefore sends a few dozen updates per root, not one per
/// directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportGate {
    reported: u64,
}

impl ReportGate {
    const GROWTH_SHIFT: u32 = 3;

    #[must_use]
    pub const fn new() -> Self {
        Self { reported: 0 }
    }

    /// Whether `bytes` should be reported; if so, it becomes the new mark.
    pub const fn admit(&mut self, bytes: u64) -> bool {
        let step = self.reported >> Self::GROWTH_SHIFT;
        if bytes > self.reported && bytes - self.reported >= step {
            self.reported = bytes;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ReportGate;

    #[test]
    fn the_first_bytes_are_reported() {
        assert!(ReportGate::new().admit(1));
    }

    #[test]
    fn nothing_new_is_not_reported() {
        let mut gate = ReportGate::new();
        assert!(!gate.admit(0));
        assert!(gate.admit(800));
        assert!(!gate.admit(800));
    }

    #[test]
    fn growth_under_an_eighth_waits_for_more() {
        let mut gate = ReportGate::new();
        assert!(gate.admit(800));
        assert!(!gate.admit(899));
        assert!(gate.admit(900));
    }

    #[test]
    fn reports_grow_geometrically() {
        let mut gate = ReportGate::new();
        let reports = (1..=1_000_000_u64)
            .filter(|&bytes| gate.admit(bytes))
            .count();
        assert!(reports < 120, "{reports} reports for a million steps");
    }
}
