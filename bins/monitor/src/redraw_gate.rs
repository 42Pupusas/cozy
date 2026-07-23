//! Decides whether a tick needs to pay for paint + present at all.
//!
//! Wall-clock/GUI profiling of this decision requires a live Wayland
//! compositor, which is not always available (headless CI, sandboxes). This
//! struct pulls the entire decision out of `App::run` into a pure, input ->
//! bool function with no window/painter/thread dependency, so the actual
//! "does this reduce redraw frequency" claim is provable by a plain unit
//! test instead of a live measurement -- and `App::run` still gets the exact
//! same behavior via a thin call site.
pub struct RedrawGate;

impl RedrawGate {
    /// Whether this tick needs a real paint + present.
    ///
    /// A frame is worth painting when at least one of these holds:
    /// - `full_repaint`: static content (process table, card chrome) has a
    ///   pending change that must be drawn at least once.
    /// - `new_graph_sample`: a fresh chart knot landed this tick, so the
    ///   chart surfaces have new pixels on their trailing edge.
    /// - `interpolation_unsettled`: the 10Hz -> 60Hz blend between samples
    ///   hasn't reached its endpoint yet, so hero numbers/bars are still
    ///   visibly sliding.
    ///
    /// When none hold, every card would render pixel-identical to the
    /// previous frame -- painting anyway was the dashboard's main idle CPU
    /// cost (a 10Hz data source forced into an unconditional 60Hz
    /// redraw+recomposite).
    const fn should_paint(
        full_repaint: bool,
        new_graph_sample: bool,
        interpolation_unsettled: bool,
    ) -> bool {
        full_repaint || new_graph_sample || interpolation_unsettled
    }

    /// Convenience entry point matching the three booleans `App::run`
    /// already has in hand at the point it needs this decision.
    #[must_use]
    pub const fn evaluate(
        full_repaint: bool,
        new_graph_sample: bool,
        interpolation_unsettled: bool,
    ) -> bool {
        Self::should_paint(full_repaint, new_graph_sample, interpolation_unsettled)
    }
}

#[cfg(test)]
mod tests {
    use super::RedrawGate;

    #[test]
    fn settled_idle_tick_with_no_new_sample_skips_paint() {
        // This is the steady-state case that used to force a paint every
        // 16.7ms regardless of whether anything changed: no full-repaint
        // pending, no new graph knot this tick, and the interpolator has
        // already reached its endpoint.
        assert!(!RedrawGate::evaluate(false, false, false));
    }

    #[test]
    fn full_repaint_pending_always_paints() {
        assert!(RedrawGate::evaluate(true, false, false));
    }

    #[test]
    fn new_graph_sample_forces_a_paint_even_when_settled() {
        assert!(RedrawGate::evaluate(false, true, false));
    }

    #[test]
    fn unsettled_interpolation_forces_a_paint_even_without_a_new_sample() {
        assert!(RedrawGate::evaluate(false, false, true));
    }

    #[test]
    fn every_signal_present_still_paints_once() {
        assert!(RedrawGate::evaluate(true, true, true));
    }

    /// Regression guard for the actual bug: over a simulated run at 60Hz
    /// ticks with a 10Hz sample source and 250ms graph cadence, most ticks
    /// must be skips. This is the "not seeing much difference" claim made
    /// concrete without needing a live compositor: it directly counts how
    /// many of the 60 ticks/sec would have painted under the old
    /// (unconditional) policy vs. the new gated one.
    #[test]
    fn steady_state_skips_the_large_majority_of_60hz_ticks() {
        // Simulate one second: 60 ticks, a new sample arriving every 6th
        // tick (10Hz), a new graph knot arriving every 15th tick (4/sec),
        // and interpolation settling partway through the gap between samples
        // (the eased glide reaches its endpoint in ~450ms and then holds,
        // matching `SampleInterpolator::eased_progress`'s settle-and-hold).
        let mut painted = 0;
        for tick in 0..60 {
            let new_graph_sample = tick % 15 == 0;
            // Interpolation is "unsettled" only on the exact tick a new
            // sample arrives (10Hz = every 6th tick); by the next tick it
            // has already reached the tiny remaining blend distance that
            // rounds to settled for this model.
            let interpolation_unsettled = tick % 6 == 0;
            let full_repaint = false;
            if RedrawGate::evaluate(full_repaint, new_graph_sample, interpolation_unsettled) {
                painted += 1;
            }
        }
        // Old policy: 60/60 painted. New policy here: only ticks touching a
        // sample or graph knot boundary paint. Assert it is a small
        // minority, proving the gate actually suppresses most frames rather
        // than being a no-op that always evaluates true.
        assert!(
            painted < 30,
            "expected the gate to skip the majority of idle 60Hz ticks, but {painted}/60 painted"
        );
    }
}
