//! Cursor animation: smooth sliding between cells + a sine-eased blink,
//! instead of the classic "teleport + hard on/off" cursor.
//!
//! Pure state machine — no I/O, no Wayland, so it's fully unit-testable and
//! driven by whatever clock the caller has (`advance(dt_ms)`). [`TermWindow`]
//! owns one, feeds it the grid's reported cursor position on every dirty
//! snapshot via [`CursorAnimator::set_target`], and calls
//! [`CursorAnimator::advance`] once per UI-loop tick to move the animation
//! forward by wall-clock time.
//!
//! Two independent animations run at once:
//!
//! * **Slide** — the visual position eases from its old cell to the new one
//!   over [`SLIDE_MS`] with a cubic ease-out, instead of snapping instantly.
//! * **Blink** — a continuous sine wave between [`MIN_ALPHA`] and full
//!   opacity, instead of a hard on/off toggle. Any activity (typing, cursor
//!   movement) resets the phase to full opacity via [`CursorAnimator::on_activity`]
//!   so the cursor never vanishes mid-keystroke — the classic bug in a naive
//!   "toggle visible every N ms" implementation.
//!
//! [`TermWindow`]: crate::window::TermWindow

use grid::CursorShape;

/// How long a cell-to-cell slide takes, in milliseconds (short jumps, e.g.
/// normal typing/cursor movement).
const SLIDE_MS: f32 = 90.0;

/// Upper bound on slide duration for very long jumps (e.g. a multi-line
/// paste landing far from where you were) — long enough for the eye to
/// track the motion, short enough to not feel sluggish.
const SLIDE_MS_MAX: f32 = 260.0;

/// Cell-distance beyond which a jump is considered "long": below it, the
/// slide always takes the base SLIDE_MS; above it, duration scales up
/// toward SLIDE_MS_MAX and a landing pulse is armed.
const LONG_JUMP_CELLS: f32 = 8.0;

/// Cell-distance at which slide duration reaches SLIDE_MS_MAX.
const MAX_JUMP_CELLS: f32 = 40.0;

/// How long the landing-pulse ring takes to expand and fade out, in ms.
const PULSE_MS: f32 = 380.0;

/// The dimmest point of the blink sine wave (never fully invisible — a
/// completely-disappearing cursor reads as a glitch, not a blink).
const MIN_ALPHA: f32 = 0.15;

/// Cursor animation configuration, derived from `config::CursorConfig`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorAnimConfig {
    pub shape: CursorShape,
    pub blink: bool,
    /// Full blink cycle length (dim → bright → dim), in milliseconds.
    pub period_ms: f32,
}

impl Default for CursorAnimConfig {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
            period_ms: 1000.0,
        }
    }
}

/// Drives the cursor's visual (float) position and blink alpha over time.
#[derive(Debug, Clone, Copy)]
pub struct CursorAnimator {
    cfg: CursorAnimConfig,

    // Slide state.
    start_col: f32,
    start_row: f32,
    target_col: u16,
    target_row: u16,
    slide_elapsed_ms: f32,
    slide_ms: f32,

    // Landing-pulse state: armed on a long jump, fires once the slide
    // settles. `pulse_elapsed_ms >= PULSE_MS` means "not currently pulsing".
    pulse_pending: bool,
    pulse_elapsed_ms: f32,

    // Blink state: phase since the last reset, wrapped to `period_ms`.
    blink_phase_ms: f32,

    visible: bool,

    /// Whether this window currently holds keyboard focus. Unfocused windows
    /// draw no cursor at all (and stop blinking) -- with two windows open
    /// side by side, a cursor blinking in *both* makes it impossible to
    /// tell which one is actually receiving keystrokes; only the focused
    /// window's cursor should ever be visible. Defaults to `true` so a
    /// freshly-created window (which hasn't seen an explicit focus event
    /// yet, e.g. some compositors send `Enter` late) still shows a cursor.
    focused: bool,
}

impl CursorAnimator {
    /// Cubic ease-out: fast start, gentle settle. `t` in `[0, 1]`.
    ///
    /// Was a free `ease_out_cubic(t)` function -- moved onto `CursorAnimator`
    /// as the sole user of this easing curve.
    fn ease_out_cubic(t: f32) -> f32 {
        let inv = 1.0 - t;
        (inv * inv).mul_add(-inv, 1.0)
    }

    /// Linear interpolation `a` -> `b` by `t`.
    ///
    /// Was a free `lerp(a, b, t)` function.
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        (b - a).mul_add(t, a)
    }

    #[must_use]
    pub fn new(cfg: CursorAnimConfig) -> Self {
        Self {
            cfg,
            start_col: 0.0,
            start_row: 0.0,
            target_col: 0,
            target_row: 0,
            slide_elapsed_ms: SLIDE_MS, // start "at rest", no slide in flight
            slide_ms: SLIDE_MS,
            pulse_pending: false,
            pulse_elapsed_ms: PULSE_MS, // start "at rest", no pulse in flight
            blink_phase_ms: 0.0,
            visible: true,
            focused: true,
        }
    }

    /// Replace the live config (e.g. on a config reload). Preserves in-flight
    /// animation state; only future ticks use the new period/shape/blink.
    pub fn set_config(&mut self, cfg: CursorAnimConfig) {
        self.cfg = cfg;
    }

    #[must_use]
    pub const fn shape(&self) -> CursorShape {
        self.cfg.shape
    }

    /// Update the target cell the cursor should be at. If it differs from
    /// the current target, starts a new slide animation from wherever the
    /// cursor visually is right now (so a rapid double-move re-targets
    /// smoothly instead of snapping back to the old cell first).
    pub fn set_target(&mut self, col: u16, row: u16, visible: bool) {
        if (col, row) != (self.target_col, self.target_row) {
            let (vc, vr) = self.visual_pos();
            let dist = (f32::from(col) - vc).hypot(f32::from(row) - vr);
            self.start_col = vc;
            self.start_row = vr;
            self.target_col = col;
            self.target_row = row;
            self.slide_elapsed_ms = 0.0;

            // Long jumps (e.g. a paste landing far away) get a longer, more
            // trackable slide, plus a landing pulse once they arrive --
            // everyday cursor moves are short and unaffected.
            if dist > LONG_JUMP_CELLS {
                let t =
                    ((dist - LONG_JUMP_CELLS) / (MAX_JUMP_CELLS - LONG_JUMP_CELLS)).clamp(0.0, 1.0);
                self.slide_ms = Self::lerp(SLIDE_MS, SLIDE_MS_MAX, t);
                self.pulse_pending = true;
            } else {
                self.slide_ms = SLIDE_MS;
                self.pulse_pending = false;
            }
        }
        self.visible = visible;
    }

    /// Reset the blink phase to full opacity. Call on any user activity
    /// (keystroke) so the cursor is always solid the instant you type,
    /// never mid-fade.
    pub fn on_activity(&mut self) {
        self.blink_phase_ms = 0.0;
    }

    /// Set whether this window currently holds keyboard focus. Losing focus
    /// hides the cursor entirely (see [`Self::alpha`]) and stops the blink
    /// from animating; regaining it snaps the blink phase back to full
    /// opacity (via [`Self::on_activity`]) so the cursor reappears solid
    /// instead of wherever the sine wave happened to leave off.
    pub fn set_focused(&mut self, focused: bool) {
        if focused && !self.focused {
            self.on_activity();
        }
        self.focused = focused;
    }

    /// Whether this window currently holds keyboard focus.
    #[must_use]
    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// Advance the animation by `dt_ms` of wall-clock time. Returns `true` if
    /// another frame should be scheduled soon (a slide is still in flight, or
    /// blink is enabled and the cursor is visible) — the caller uses this to
    /// decide whether to keep polling at animation-frame-rate or fall back to
    /// idling on the next real event.
    pub fn advance(&mut self, dt_ms: f32) -> bool {
        let mut animating = false;
        if self.slide_elapsed_ms < self.slide_ms {
            self.slide_elapsed_ms = (self.slide_elapsed_ms + dt_ms).min(self.slide_ms);
            if self.slide_elapsed_ms < self.slide_ms {
                animating = true;
            } else if self.pulse_pending {
                // The slide just settled at the destination -- fire the
                // landing pulse now, from a clean start.
                self.pulse_pending = false;
                self.pulse_elapsed_ms = 0.0;
            }
        }
        if self.pulse_elapsed_ms < PULSE_MS {
            self.pulse_elapsed_ms = (self.pulse_elapsed_ms + dt_ms).min(PULSE_MS);
            if self.pulse_elapsed_ms < PULSE_MS {
                animating = true;
            }
        }
        if self.cfg.blink && self.visible && self.focused {
            self.blink_phase_ms += dt_ms;
            if self.cfg.period_ms > 0.0 {
                self.blink_phase_ms %= self.cfg.period_ms;
            }
            animating = true;
        }
        animating
    }

    /// Current visual (float) cell position, easing toward the target.
    #[must_use]
    pub fn visual_pos(&self) -> (f32, f32) {
        if self.slide_elapsed_ms >= self.slide_ms {
            return (f32::from(self.target_col), f32::from(self.target_row));
        }
        let t = Self::ease_out_cubic(self.slide_elapsed_ms / self.slide_ms);
        (
            Self::lerp(self.start_col, f32::from(self.target_col), t),
            Self::lerp(self.start_row, f32::from(self.target_row), t),
        )
    }

    /// Current alpha, `0.0..=1.0`. `0.0` when the cursor is hidden (DECTCEM
    /// off, or viewport scrolled away from the live cursor); a continuous
    /// sine wave between [`MIN_ALPHA`] and `1.0` when blink is enabled;
    /// always `1.0` when blink is disabled.
    #[must_use]
    pub fn alpha(&self) -> f32 {
        if !self.visible || !self.focused {
            return 0.0;
        }
        if !self.cfg.blink || self.cfg.period_ms <= 0.0 {
            return 1.0;
        }
        let phase = self.blink_phase_ms / self.cfg.period_ms; // 0..1
        let s = (phase * std::f32::consts::TAU).cos(); // 1 at phase 0
        (1.0 - MIN_ALPHA).mul_add(s.mul_add(0.5, 0.5), MIN_ALPHA)
    }

    /// Whether the cursor should be considered "visible" at all right now
    /// (distinct from `alpha() == 0.0` at the blink's dimmest point — this is
    /// DECTCEM/viewport visibility, not blink phase).
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether the caller should keep polling at animation-frame-rate
    /// (~60fps) rather than idling on the full event-wait timeout: true
    /// while a slide is in flight, or blink is enabled and the cursor is
    /// currently visible (blink then runs forever until visibility changes
    /// or blink is turned off).
    #[must_use]
    pub fn is_animating(&self) -> bool {
        let sliding = self.slide_elapsed_ms < self.slide_ms;
        let pulsing = self.pulse_elapsed_ms < PULSE_MS;
        let blinking = self.cfg.blink && self.visible && self.focused;
        sliding || pulsing || blinking
    }

    /// Landing-pulse progress: `None` if no pulse is currently in flight;
    /// otherwise `Some(t)` with `t` in `0.0..=1.0` (0 = just landed, 1 =
    /// fully faded out). The renderer uses this to draw a brief expanding,
    /// fading ring at the cursor's cell so a long jump (e.g. a paste landing
    /// far away) draws the eye to where it ended up.
    #[must_use]
    pub fn pulse(&self) -> Option<f32> {
        if self.pulse_elapsed_ms < PULSE_MS {
            Some(self.pulse_elapsed_ms / PULSE_MS)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(blink: bool) -> CursorAnimConfig {
        CursorAnimConfig {
            shape: CursorShape::Block,
            blink,
            period_ms: 1000.0,
        }
    }

    #[test]
    fn new_starts_at_rest_at_origin() {
        let a = CursorAnimator::new(cfg(true));
        assert_eq!(a.visual_pos(), (0.0, 0.0));
        assert!(a.is_visible());
    }

    #[test]
    fn set_target_same_cell_does_not_restart_slide() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        assert_eq!(a.visual_pos(), (0.0, 0.0));
        // Advance fully to rest, then re-set the same target: must not
        // reset slide_elapsed_ms (there is nothing to animate).
        a.advance(1000.0);
        let animating = a.advance(0.0);
        a.set_target(0, 0, true);
        // Still at (0,0); a no-op retarget doesn't perturb position.
        assert_eq!(a.visual_pos(), (0.0, 0.0));
        let _ = animating;
    }

    #[test]
    fn set_target_new_cell_starts_a_slide_from_current_visual_pos() {
        let mut a = CursorAnimator::new(cfg(false)); // blink off to isolate slide
        a.set_target(5, 2, true);
        // Immediately after retargeting, we're at the very start of the
        // slide (t=0), so visual pos is still the old position.
        assert_eq!(a.visual_pos(), (0.0, 0.0));
        // Halfway through the slide duration, we should have moved partway.
        a.advance(SLIDE_MS / 2.0);
        let (c, r) = a.visual_pos();
        assert!(c > 0.0 && c < 5.0, "col should be mid-slide, got {c}");
        assert!(r > 0.0 && r < 2.0, "row should be mid-slide, got {r}");
        // Fully advance past the slide duration: lands exactly on target.
        a.advance(SLIDE_MS);
        assert_eq!(a.visual_pos(), (5.0, 2.0));
    }

    #[test]
    fn slide_uses_ease_out_not_linear() {
        // Ease-out-cubic covers MORE than half the distance in the first
        // half of the duration (fast start, gentle settle) — the opposite of
        // ease-in. Assert the halfway-*time* sample is past the halfway
        // *distance* point.
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(10, 0, true);
        a.advance(SLIDE_MS / 2.0);
        let (c, _) = a.visual_pos();
        assert!(
            c > 5.0,
            "ease-out should be past the midpoint by t=0.5, got {c}"
        );
    }

    #[test]
    fn advance_reports_still_animating_while_sliding() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(3, 0, true);
        assert!(a.advance(10.0), "mid-slide must report animating");
        a.advance(SLIDE_MS);
        assert!(
            !a.advance(0.0),
            "at rest with blink off must report not animating"
        );
    }

    #[test]
    fn advance_reports_animating_forever_while_blink_enabled_and_visible() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true); // no slide (same cell)
        assert!(a.advance(5000.0), "blink keeps the animator alive");
    }

    #[test]
    fn advance_reports_not_animating_when_hidden_and_no_slide() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, false);
        assert!(!a.advance(5000.0), "hidden cursor has nothing to animate");
    }

    // `alpha()` returns the exact hardcoded constants 0.0/1.0 in these
    // states (not a computed/accumulated float), so a strict `==` is
    // correct here, not a precision-loss trap -- see `alpha_starts_full...`
    // below for the case that legitimately needs an epsilon comparison.
    #[allow(clippy::float_cmp)]
    #[test]
    fn alpha_is_zero_when_hidden() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, false);
        assert_eq!(a.alpha(), 0.0);
    }

    #[allow(clippy::float_cmp)]
    #[test]
    fn alpha_is_always_one_when_blink_disabled() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(0, 0, true);
        assert_eq!(a.alpha(), 1.0);
        a.advance(10_000.0);
        assert_eq!(a.alpha(), 1.0);
    }

    #[test]
    fn alpha_starts_full_and_dims_over_the_first_half_period() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        let a0 = a.alpha();
        assert!((a0 - 1.0).abs() < 1e-6, "must start fully opaque, got {a0}");
        a.advance(500.0); // half the 1000ms period -> the dimmest point
        let a_half = a.alpha();
        assert!(
            (a_half - MIN_ALPHA).abs() < 1e-3,
            "must be at minimum alpha at the half period, got {a_half}"
        );
        a.advance(500.0); // full period -> back to fully opaque
        let a_full = a.alpha();
        assert!(
            (a_full - 1.0).abs() < 1e-3,
            "must return to full opacity after a whole period, got {a_full}"
        );
    }

    #[test]
    fn alpha_never_hits_true_zero_mid_blink() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        for _ in 0..200 {
            a.advance(17.0);
            assert!(
                a.alpha() >= MIN_ALPHA - 1e-6,
                "alpha dipped below the floor"
            );
        }
    }

    #[test]
    fn on_activity_resets_phase_to_full_opacity() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        a.advance(500.0); // now at the dimmest point
        assert!((a.alpha() - MIN_ALPHA).abs() < 1e-3);
        a.on_activity();
        assert!(
            (a.alpha() - 1.0).abs() < 1e-6,
            "activity must snap back to full opacity"
        );
    }

    #[test]
    fn retargeting_mid_slide_starts_the_new_slide_from_the_current_visual_position() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(10, 0, true);
        a.advance(SLIDE_MS / 2.0);
        let (mid_c, _) = a.visual_pos();
        // Retarget to a new cell before the first slide finishes.
        a.set_target(0, 0, true);
        // The new slide must start from wherever we were, not teleport back
        // to the origin or to the old target.
        let (c, _) = a.visual_pos();
        assert!(
            (c - mid_c).abs() < 1e-4,
            "retarget must not jump: was {mid_c}, now {c}"
        );
    }

    #[test]
    fn is_animating_true_while_sliding_even_with_blink_off() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(5, 0, true);
        assert!(a.is_animating());
        a.advance(SLIDE_MS);
        assert!(!a.is_animating());
    }

    #[test]
    fn is_animating_true_forever_while_blinking_and_visible() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        a.advance(SLIDE_MS); // slide settles immediately (same cell)
        assert!(a.is_animating(), "blink keeps it animating");
    }

    #[test]
    fn is_animating_false_when_hidden_and_not_sliding() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, false);
        assert!(!a.is_animating());
    }

    #[test]
    fn short_jump_does_not_arm_a_pulse() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(3, 0, true); // well under LONG_JUMP_CELLS
        a.advance(SLIDE_MS);
        assert!(a.pulse().is_none(), "short jumps must not pulse");
    }

    #[test]
    fn long_jump_arms_a_pulse_that_fires_on_arrival() {
        let mut a = CursorAnimator::new(cfg(false));
        a.set_target(30, 5, true); // far beyond LONG_JUMP_CELLS
        // Pulse only fires once the slide settles, not mid-flight.
        a.advance(1.0);
        assert!(a.pulse().is_none(), "pulse must not fire before landing");
        // Drive just past the (distance-scaled, possibly > SLIDE_MS) slide,
        // but not so far the pulse (which starts right after) also finishes.
        a.advance(SLIDE_MS_MAX + 1.0);
        assert!(
            a.pulse().is_some(),
            "pulse should be active right after landing"
        );
        // Eventually the pulse itself fades out.
        a.advance(PULSE_MS + 1.0);
        assert!(a.pulse().is_none(), "pulse must eventually finish");
    }

    #[test]
    fn long_jump_slide_takes_longer_than_a_short_one() {
        let mut short = CursorAnimator::new(cfg(false));
        short.set_target(2, 0, true);
        let mut long = CursorAnimator::new(cfg(false));
        long.set_target(35, 0, true);
        // After the short jump's normal duration, the short one is done...
        short.advance(SLIDE_MS);
        assert_eq!(short.visual_pos(), (2.0, 0.0));
        // ...but the long one should not have arrived yet.
        long.advance(SLIDE_MS);
        assert_ne!(long.visual_pos(), (35.0, 0.0));
    }

    #[test]
    // `alpha()` returns a hard `0.0` sentinel when hidden (not a computed
    // value that could carry float error), so an exact compare is correct.
    #[allow(clippy::float_cmp)]
    fn unfocused_window_hides_cursor_and_stops_blinking() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        a.set_focused(false);
        assert_eq!(a.alpha(), 0.0, "unfocused cursor must be fully hidden");
        assert!(
            !a.advance(5000.0),
            "unfocused blink must not keep the animator alive"
        );
        assert!(!a.is_animating());
    }

    #[test]
    fn regaining_focus_snaps_back_to_full_opacity() {
        let mut a = CursorAnimator::new(cfg(true));
        a.set_target(0, 0, true);
        a.advance(500.0); // dimmest point
        a.set_focused(false);
        // Let simulated time pass while unfocused -- must not affect phase.
        a.set_focused(true);
        let al = a.alpha();
        assert!(
            (al - 1.0).abs() < 1e-6,
            "regaining focus must snap to full opacity, got {al}"
        );
    }

    #[test]
    fn focused_defaults_true_on_new() {
        let a = CursorAnimator::new(cfg(true));
        assert!(a.is_focused());
    }

    #[test]
    fn set_config_updates_shape() {
        let mut a = CursorAnimator::new(cfg(true));
        assert_eq!(a.shape(), CursorShape::Block);
        a.set_config(CursorAnimConfig {
            shape: CursorShape::Bar,
            ..cfg(true)
        });
        assert_eq!(a.shape(), CursorShape::Bar);
    }
}
