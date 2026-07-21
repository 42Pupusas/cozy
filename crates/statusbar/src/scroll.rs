//! Wheel-scroll-to-adjust accumulation: raw `wl_pointer.axis` pixels in,
//! whole "detents" (percentage-point steps) out.
//!
//! Pulled out of `bins/statusbar/src/main.rs::Modules` into the library
//! crate so the accumulation math is unit-testable without a live Wayland
//! connection. This exact math went through three broken iterations before
//! landing right (see the module doc below) -- exactly the kind of "small
//! arithmetic that's obviously correct until a touchpad sends events an
//! order of magnitude smaller than a mouse wheel" bug a unit test catches
//! immediately and a screenshot doesn't.

/// Raw wheel pixels (`wl_fixed`-derived) per one logical scroll "step" /
/// detent.
///
/// Matches a typical physical mouse wheel click; touchpad/hi-res-wheel
/// micro-events accumulate fractionally toward this rather than each
/// counting as a full step on their own.
pub const PX_PER_STEP: i32 = 15;

/// Accumulates raw scroll-wheel pixels for a single control until there's
/// at least one whole detent's worth.
///
/// Then hands back the percentage-point delta to apply -- carrying any
/// sub-detent remainder forward rather than dropping it.
///
/// # History
///
/// This type replaces two earlier, broken versions of the same idea:
///
/// 1. **Per-event flooring**: converting each individual `wl_pointer.axis`
///    event to at least one step via `.max(1)` before summing. A single
///    physical scroll gesture (especially kinetic/smooth scrolling on a
///    touchpad, or a high-resolution mouse wheel) delivers *many* small
///    axis events under one detent each -- flooring every one of them up
///    to a full step meant a gentle scroll registered as a dozen-plus
///    steps, and the value jumped around inconsistently.
/// 2. **Per-event pixel truncation**: `Fixed::to_pixels()` (plain
///    truncating integer division) applied to each event independently,
///    *before* accumulation. `wl_fixed`'s 8 fractional bits mean e.g. a
///    0.3px scroll is a real, distinct value on the wire -- truncating it
///    to 0px and discarding it meant a slow/gentle scroll could produce
///    nothing but zeros forever (this is why brightness scrolling looked
///    entirely broken while volume scrolling merely looked janky: the
///    volume wheel/touchpad events in testing happened to be coarser).
///
/// The fix for both: accumulate in the finest-grained unit available (raw
/// pixels here; `BarWindow` itself separately carries the `wl_fixed`
/// sub-pixel remainder forward before ever handing pixels to this type),
/// and only convert to a step once a whole detent's worth has built up.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScrollAccumulator {
    pending_px: i32,
}

impl ScrollAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self { pending_px: 0 }
    }

    /// Add `raw_px` to the pending accumulation.
    pub const fn add(&mut self, raw_px: i32) {
        self.pending_px += raw_px;
    }

    /// Whether any pixels are queued (whole detent or not).
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending_px != 0
    }

    /// Convert whatever whole detents' worth of pixels have accumulated
    /// into a step count (positive = wheel down, negative = wheel up,
    /// matching `wl_pointer.axis`'s own sign convention), leaving any
    /// sub-detent remainder queued. Returns `0` (and leaves the
    /// accumulator untouched) if less than one whole detent is pending.
    pub const fn take_steps(&mut self) -> i32 {
        let steps = self.pending_px / PX_PER_STEP;
        if steps == 0 {
            return 0;
        }
        self.pending_px -= steps * PX_PER_STEP;
        steps
    }

    /// Reset to empty, discarding any pending sub-detent remainder --
    /// e.g. when the pointer moves to a different segment, whose scroll
    /// history isn't relevant to this one.
    pub const fn reset(&mut self) {
        self.pending_px = 0;
    }
}

/// Convert a wheel step count into a percentage-point delta to apply to a
/// volume/brightness control.
///
/// Wheel up (negative steps, matching `wl_pointer.axis`'s sign convention
/// where negative = up/away) increases the value; wheel down decreases it
/// -- the usual volume-knob feel (push up = louder).
#[must_use]
pub const fn steps_to_delta_pct(steps: i32, step_pct: i32) -> i32 {
    -steps * step_pct
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_small_event_stays_pending() {
        let mut acc = ScrollAccumulator::new();
        acc.add(3);
        assert_eq!(acc.take_steps(), 0);
        assert!(acc.is_pending());
    }

    #[test]
    fn many_small_events_eventually_produce_one_step() {
        let mut acc = ScrollAccumulator::new();
        // Five 3px touchpad micro-events = 15px = exactly one detent.
        for _ in 0..5 {
            acc.add(3);
        }
        assert_eq!(acc.take_steps(), 1);
        assert!(!acc.is_pending());
    }

    #[test]
    fn gentle_scroll_does_not_produce_a_dozen_steps() {
        // Regression test for the "per-event .max(1) flooring" bug: ten
        // 1px events must NOT produce ten steps, only zero (10 < 15px).
        let mut acc = ScrollAccumulator::new();
        for _ in 0..10 {
            acc.add(1);
        }
        assert_eq!(acc.take_steps(), 0);
    }

    #[test]
    fn remainder_carries_forward_across_calls() {
        let mut acc = ScrollAccumulator::new();
        acc.add(20); // 1 step, 5px remainder
        assert_eq!(acc.take_steps(), 1);
        acc.add(10); // 5 + 10 = 15px = 1 more step
        assert_eq!(acc.take_steps(), 1);
        assert!(!acc.is_pending());
    }

    #[test]
    fn negative_pixels_produce_negative_steps() {
        let mut acc = ScrollAccumulator::new();
        acc.add(-30);
        assert_eq!(acc.take_steps(), -2);
    }

    #[test]
    fn reset_discards_remainder() {
        let mut acc = ScrollAccumulator::new();
        acc.add(10);
        acc.reset();
        acc.add(4);
        // 10 + 4 would've been 14 (< 1 step) either way here, so use a
        // value that would only pass if the reset actually happened.
        acc.add(1); // now at 5px total post-reset, not 15
        assert_eq!(acc.take_steps(), 0);
    }

    #[test]
    fn steps_to_delta_pct_flips_sign() {
        // Wheel up (negative steps) => positive (increase) delta.
        assert_eq!(steps_to_delta_pct(-1, 5), 5);
        // Wheel down (positive steps) => negative (decrease) delta.
        assert_eq!(steps_to_delta_pct(1, 5), -5);
        assert_eq!(steps_to_delta_pct(0, 5), 0);
        assert_eq!(steps_to_delta_pct(-3, 5), 15);
    }

    #[test]
    fn zero_add_is_noop() {
        let mut acc = ScrollAccumulator::new();
        acc.add(0);
        assert!(!acc.is_pending());
        assert_eq!(acc.take_steps(), 0);
    }
}
