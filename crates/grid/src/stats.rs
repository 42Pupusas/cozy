//! Scroll-path instrumentation for the benchmark harness.
//!
//! The harness needs to know *why* the scroll path is expensive: how often
//! the floating-window compaction in `Screen::ensure_tail_room` fires, how
//! many rows each one moves, and how many rows get blanked by scrolls.
//!
//! The `ScrollStats` *type* is always compiled (it is a plain POD that
//! benchmark code names in signatures in both feature modes), but the
//! *state* -- the counter field on `Screen` and the recording in the hot
//! path -- exists only under the `stats` feature, so the default build
//! carries no dead weight: the recording methods have no-op twins.

/// Counters describing the work done by the scroll/eviction path.
///
/// All counts are cumulative since screen construction (or the last
/// [`ScrollStats::reset`]). Units are rows unless stated otherwise; row
/// counts multiply by `cols * size_of::<Cell>()` to get bytes touched.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScrollStats {
    /// Number of `scroll_up_impl` invocations (one per LF past the bottom
    /// margin in a flood workload).
    pub scroll_up_calls: u64,
    /// Rows blanked by scrolls (the freshly-exposed bottom rows).
    pub rows_blanked: u64,
    /// Number of times `ensure_tail_room` had to compact (slide the live
    /// window back to physical row 0).
    pub compactions: u64,
    /// Total rows copied by those compactions -- the real memmove volume.
    pub compaction_rows_moved: u64,
}

impl ScrollStats {
    /// Record one scroll that blanked `rows` freshly-exposed rows.
    pub const fn note_scroll(&mut self, rows: u64) {
        self.scroll_up_calls += 1;
        self.rows_blanked += rows;
    }

    /// Record one compaction that moved `rows` live rows back to the front.
    pub const fn note_compaction(&mut self, rows: u64) {
        self.compactions += 1;
        self.compaction_rows_moved += rows;
    }

    /// Zero every counter (e.g. between benchmark phases).
    pub const fn reset(&mut self) {
        // Written out field-by-field (not `Self::default()`) so this stays a
        // `const fn`; `Default::default()` is not callable in const context.
        self.scroll_up_calls = 0;
        self.rows_blanked = 0;
        self.compactions = 0;
        self.compaction_rows_moved = 0;
    }
}
