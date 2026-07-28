//! A forward-progress watchdog for completion-driven loops.
//!
//! # Why this exists
//!
//! [`conn::Reader::run`](crate::conn::Reader::run) is a classic
//! arm/reap/re-arm `io_uring` loop: block in `io_uring_enter`, drain the
//! completions, push whatever arrived onto the SPSC ring, re-arm, repeat.
//! Every iteration is *supposed* to block, because a healthy socket only
//! completes when the compositor actually sent something.
//!
//! That assumption breaks the moment the peer goes away in a way the loop
//! doesn't recognise as EOF. A recv against a dead socket completes
//! **immediately** and forever: `submit_and_wait(1)` returns at once, the
//! completion carries no provided buffer, nothing is absorbed into the
//! inbox, nothing is pushed to the producer, and the loop re-arms into the
//! same instant completion. The result is a thread spinning `io_uring_enter`
//! at tens of millions of submissions per second — a fully pinned core — on
//! a process whose main thread is parked in `futex_do_wait` and will never
//! be woken, because waking requires a push that never happens.
//!
//! We observed exactly this: four daemons (`wallpaper`, `clipboard`,
//! `monitor`, `shot`) orphaned by a compositor restart, each burning a whole
//! core, their rx ring's `SqHead` advancing ~48 million times in a couple of
//! seconds while `CqHead == CqTail`.
//!
//! # Why a guard rather than only fixing the EOF check
//!
//! Widening the EOF condition fixes *that* trigger. It does not make the
//! loop structurally incapable of spinning — the next unhandled completion
//! shape reintroduces the same core-eater, silently, in a background thread
//! nobody is watching. This guard is the backstop: it makes "the loop went
//! round and round achieving nothing" a bounded, self-terminating condition
//! instead of an unbounded one. Belt (correct EOF) *and* braces (this).
//!
//! It deliberately counts *consecutive* barren iterations rather than using
//! a rate or a clock. A wall-clock budget would need a time source in a hot
//! loop and would misfire on a legitimately slow-but-progressing link; a
//! consecutive count is free, allocation-free, and has an exact meaning:
//! "N completions in a row taught us nothing". A healthy connection resets
//! the counter constantly, so the threshold can sit high enough that no
//! plausible real traffic pattern trips it.

/// What [`ProgressGuard::record`] concluded about the loop's health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// Keep going — either this iteration did something useful, or we have
    /// not yet seen enough barren ones in a row to call it.
    Continue,
    /// The loop has completed [`ProgressGuard::limit`] iterations in a row
    /// without absorbing bytes or pushing events. It is spinning against a
    /// peer that will never say anything useful again; the caller should
    /// tear the loop down rather than keep burning a core.
    Wedged,
}

/// Counts consecutive no-op iterations of a completion loop and reports when
/// that count crosses a threshold.
///
/// Owns exactly one piece of state (the barren-iteration streak) and one
/// policy knob (the threshold), which is the whole reason it's a struct
/// rather than a stray `usize` threaded through `run`: the "did we make
/// progress" bookkeeping is a self-contained responsibility with its own
/// invariants, and it is unit-testable in isolation from any actual socket.
#[derive(Debug, Clone)]
pub struct ProgressGuard {
    /// Consecutive iterations that absorbed nothing and pushed nothing.
    barren: u32,
    /// How many barren iterations in a row before we declare the loop wedged.
    limit: u32,
}

impl ProgressGuard {
    /// Default threshold.
    ///
    /// Sized to be unreachable by healthy traffic but to trip essentially
    /// instantly once a spin starts. A working connection cannot produce
    /// 4096 consecutive completions that carry no buffer and push no event —
    /// it blocks in the kernel long before that. A wedged one reaches 4096
    /// in well under a millisecond (we measured ~48M spins/sec), so the
    /// guard costs a few microseconds of the very burn it exists to stop,
    /// while leaving enormous headroom against false positives (e.g. a burst
    /// of pure timer re-arms, or buffer-exhaustion completions we recycle
    /// without absorbing).
    pub const DEFAULT_LIMIT: u32 = 4096;

    /// A guard using [`Self::DEFAULT_LIMIT`].
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limit(Self::DEFAULT_LIMIT)
    }

    /// A guard with an explicit threshold — used by tests to trip the guard
    /// without spinning four thousand times.
    ///
    /// A `limit` of 0 is clamped to 1: "wedged after zero barren iterations"
    /// would mean the guard fires before the loop has done anything at all,
    /// which is never what a caller means.
    #[must_use]
    pub const fn with_limit(limit: u32) -> Self {
        Self {
            barren: 0,
            limit: if limit == 0 { 1 } else { limit },
        }
    }

    /// The configured threshold.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }

    /// How many barren iterations have occurred back-to-back right now.
    /// Exposed mainly so tests can assert the reset behaviour precisely.
    #[must_use]
    pub const fn barren_streak(&self) -> u32 {
        self.barren
    }

    /// Record one iteration of the loop.
    ///
    /// `made_progress` should be true when the iteration did something
    /// observable — absorbed bytes into the inbox, or pushed an event to the
    /// consumer. Anything else (a completion we couldn't interpret, a
    /// re-arm that instantly completed against a dead peer) is barren.
    ///
    /// Any progress at all resets the streak, so this only ever fires on an
    /// *uninterrupted* run of futility.
    pub const fn record(&mut self, made_progress: bool) -> Progress {
        if made_progress {
            self.barren = 0;
            return Progress::Continue;
        }
        // Saturating so a guard that is somehow polled past its limit
        // without the caller acting cannot wrap around and go quiet again.
        self.barren = self.barren.saturating_add(1);
        if self.barren >= self.limit {
            Progress::Wedged
        } else {
            Progress::Continue
        }
    }

    /// Forget the current streak without recording an iteration. For callers
    /// that know an upcoming quiet stretch is legitimate.
    pub const fn reset(&mut self) {
        self.barren = 0;
    }
}

impl Default for ProgressGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_guard_starts_with_no_streak() {
        let guard = ProgressGuard::new();
        assert_eq!(guard.barren_streak(), 0);
        assert_eq!(guard.limit(), ProgressGuard::DEFAULT_LIMIT);
    }

    #[test]
    fn productive_iterations_never_wedge() {
        let mut guard = ProgressGuard::with_limit(3);
        // Far more iterations than the limit, all productive.
        for _ in 0..1_000 {
            assert_eq!(guard.record(true), Progress::Continue);
            assert_eq!(guard.barren_streak(), 0);
        }
    }

    #[test]
    fn wedges_exactly_at_the_limit() {
        let mut guard = ProgressGuard::with_limit(3);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Continue);
        // Third consecutive barren iteration hits limit == 3.
        assert_eq!(guard.record(false), Progress::Wedged);
    }

    #[test]
    fn any_progress_resets_the_streak() {
        let mut guard = ProgressGuard::with_limit(3);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.barren_streak(), 2);

        // One good iteration wipes the slate...
        assert_eq!(guard.record(true), Progress::Continue);
        assert_eq!(guard.barren_streak(), 0);

        // ...so it takes a fresh full run of barren ones to wedge.
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Wedged);
    }

    #[test]
    fn alternating_progress_never_wedges() {
        // The pathological-but-legitimate case: a link that only manages
        // useful work every other completion must never be torn down.
        let mut guard = ProgressGuard::with_limit(2);
        for _ in 0..1_000 {
            assert_eq!(guard.record(false), Progress::Continue);
            assert_eq!(guard.record(true), Progress::Continue);
        }
    }

    #[test]
    fn stays_wedged_once_past_the_limit() {
        // A caller that ignores the first Wedged must keep seeing it, not
        // have the counter wrap and silently go quiet again.
        let mut guard = ProgressGuard::with_limit(2);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Wedged);
        for _ in 0..1_000 {
            assert_eq!(guard.record(false), Progress::Wedged);
        }
    }

    #[test]
    fn saturates_instead_of_wrapping() {
        let mut guard = ProgressGuard::with_limit(1);
        guard.barren = u32::MAX - 1;
        assert_eq!(guard.record(false), Progress::Wedged);
        assert_eq!(guard.barren_streak(), u32::MAX);
        // Already saturated: recording again must not wrap to 0.
        assert_eq!(guard.record(false), Progress::Wedged);
        assert_eq!(guard.barren_streak(), u32::MAX);
    }

    #[test]
    fn zero_limit_is_clamped_to_one() {
        let mut guard = ProgressGuard::with_limit(0);
        assert_eq!(guard.limit(), 1);
        // Clamped, so the very first barren iteration wedges -- but it does
        // take an iteration; the guard never fires before doing any work.
        assert_eq!(guard.record(false), Progress::Wedged);
    }

    #[test]
    fn explicit_reset_clears_the_streak() {
        let mut guard = ProgressGuard::with_limit(3);
        assert_eq!(guard.record(false), Progress::Continue);
        assert_eq!(guard.record(false), Progress::Continue);
        guard.reset();
        assert_eq!(guard.barren_streak(), 0);
        assert_eq!(guard.record(false), Progress::Continue);
    }

    #[test]
    fn default_matches_new() {
        let a = ProgressGuard::default();
        let b = ProgressGuard::new();
        assert_eq!(a.limit(), b.limit());
        assert_eq!(a.barren_streak(), b.barren_streak());
    }
}
