//! FPS / frame-pacing debug instrumentation for [`crate::window::TermWindow`].
//!
//! Entirely behind the crate's `trace` feature (see `Cargo.toml`): a release
//! build carries none of this state and none of the per-frame branches that
//! read it. Split out of `window.rs` so the window module isn't also the
//! place this orthogonal, opt-in diagnostics concern lives and is tested.

#![cfg(feature = "trace")]

use crate::render;

/// Debug FPS instrumentation mode -- see [`DebugOverlay::mode`], the
/// `debug` field on [`crate::window::TermWindow`]. Set once at startup from
/// the `TERMINAL_DEBUG_FPS` environment variable and fixed for the window's
/// lifetime.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DebugFps {
    /// No instrumentation -- zero cost (one enum compare per frame).
    Off,
    /// Draw a live "NN.N fps" readout in the top-right corner every frame.
    Overlay,
    /// Print "terminal: NN.N fps" to stderr about once a second.
    Log,
}

impl DebugFps {
    /// `TERMINAL_DEBUG_FPS=1` / `=true` → [`Self::Overlay`];
    /// `TERMINAL_DEBUG_FPS=log` → [`Self::Log`]; unset or anything else →
    /// [`Self::Off`].
    fn from_env() -> Self {
        match std::env::var("TERMINAL_DEBUG_FPS") {
            Ok(v) if v == "log" => Self::Log,
            Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => Self::Overlay,
            _ => Self::Off,
        }
    }
}

/// FPS/frame-pacing debug instrumentation, extracted from [`crate::window::TermWindow`]
/// so the "is this thing even on" mode flag, its rolling stats, and the
/// overlay text's own dirty-row bookkeeping live and are tested in one place
/// instead of being three separate fields on the window god-struct. Fixed
/// for the window's lifetime (mode is read once from the environment at
/// startup).
pub struct DebugOverlay {
    mode: DebugFps,
    stats: FrameStats,
    /// Viewport rows the "NN.N fps" overlay text touched last frame --
    /// repainted clean before redrawing it, the same idempotent scheme as
    /// `prev_cursor_rows`/`prev_selection_rows` on `TermWindow`.
    pub(crate) prev_rows: Vec<u16>,
}

impl DebugOverlay {
    /// Mode fixed once at startup from `TERMINAL_DEBUG_FPS`; stats and
    /// overlay-row tracking start empty.
    pub(crate) fn new() -> Self {
        Self {
            mode: DebugFps::from_env(),
            stats: FrameStats::new(),
            prev_rows: Vec::new(),
        }
    }

    /// Largest `pending_dirty` backlog seen since the last log -- forwarded
    /// to [`FrameStats::note_backlog`].
    pub(crate) fn note_backlog(&mut self, depth: usize) {
        self.stats.note_backlog(depth);
    }

    /// Record that `maybe_present` bailed because the compositor hadn't yet
    /// sent the frame callback (vsync pacing, not buffer starvation).
    pub(crate) const fn note_skipped_not_ready(&mut self) {
        self.stats.skipped_not_ready += 1;
    }

    /// Record that `maybe_present` bailed because both shm buffers were still
    /// held by the compositor (buffer starvation, not vsync pacing).
    pub(crate) const fn note_skipped_no_slot(&mut self) {
        self.stats.skipped_no_slot += 1;
    }

    /// Record a present that just happened, for the rolling fps average.
    pub(crate) fn record_present(&mut self) {
        self.stats.record();
    }

    /// Add the CPU time one `maybe_present` spent on real work (not blocked
    /// on the compositor).
    pub(crate) fn record_busy(&mut self, dur: std::time::Duration) {
        self.stats.record_busy(dur);
    }

    /// Debug FPS instrumentation: draw a "NN.N fps" readout into the top-right
    /// corner of `backbuffer` (`DebugFps::Overlay`); no-op (and essentially
    /// free -- one enum compare) for `Off`/`Log`, since the `Log` readout goes
    /// to stderr via [`Self::log_frame_pacing`] instead. Returns the viewport
    /// rows the overlay text touched, for `prev_rows` bookkeeping (same
    /// idempotent repaint scheme as the cursor/selection overlays).
    pub(crate) fn draw(
        &mut self,
        backbuffer: &mut [u32],
        width: i32,
        height: i32,
        cell_w: i32,
        renderer: &mut render::Renderer,
    ) -> Vec<u16> {
        if self.mode != DebugFps::Overlay {
            return Vec::new();
        }
        self.stats.prune(std::time::Instant::now());
        let text = format!("{:.1} fps", self.stats.fps());
        // `width`/`cell_w` are pixel dimensions, and the readout text is a
        // handful of ASCII chars -- both always far under `u16::MAX`.
        let cols = u16::try_from((width / cell_w).max(1)).unwrap_or(u16::MAX);
        let text_cols = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
        if text_cols == 0 || text_cols > cols {
            return Vec::new();
        }
        let start_col = cols - text_cols;
        // High-contrast fixed readout: white on black, independent of the
        // active theme, so it's always legible regardless of what's underneath.
        let attrs = grid::Attrs::with_colors(
            grid::Color::Rgb(0xff, 0xff, 0xff),
            grid::Color::Rgb(0x00, 0x00, 0x00),
        );
        let mut canvas = crate::draw::Canvas::new(backbuffer, width, height);
        for (i, ch) in text.chars().enumerate() {
            let cell = grid::Cell::new(ch, attrs, 0);
            renderer.blit_cell(
                &mut canvas,
                start_col + u16::try_from(i).unwrap_or(u16::MAX),
                0,
                &cell,
            );
        }
        vec![0u16]
    }

    /// Emit the frame-pacing diagnostic to stderr ~once a second when
    /// `TERMINAL_DEBUG_FPS=log`. Called at the *top* of every `maybe_present`
    /// -- crucially including the calls that early-return without presenting
    /// (`skipped_not_ready` / `skipped_no_slot`) -- so the counter split stays
    /// live even under a hard stall, when successful presents (and thus the
    /// old log site in `draw`) go quiet exactly when the numbers matter most.
    ///
    /// The line answers the scroll-flood question directly:
    /// - `skipped no-slot`  ≫ `not-ready`  → buffer starvation (both shm
    ///   frames held by the compositor) → more frames would help (triple
    ///   buffering).
    /// - `skipped not-ready` ≫ `no-slot`   → pure vsync pacing (waiting on
    ///   `wl_callback.done`); more buffers won't help.
    /// - `-> CPU-bound`                     → render work itself is the wall.
    pub(crate) fn log_frame_pacing(&mut self) {
        if self.mode != DebugFps::Log {
            return;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.stats.last_log).as_secs_f32() < 1.0 {
            return;
        }
        self.stats.last_log = now;
        self.stats.prune(now);
        let fps = self.stats.fps();
        let busy_us = self.stats.drain_busy_us();
        // Frame interval at this fps, in microseconds. `busy_us` is CPU time
        // actually spent in maybe_present; the rest of the interval was spent
        // blocked on `wl_callback.done`.
        let interval_us = if fps > 0.0 {
            1_000_000.0 / f64::from(fps)
        } else {
            0.0
        };
        let bound = if interval_us > 0.0 && busy_us / interval_us > 0.6 {
            "CPU-bound"
        } else {
            "compositor-bound"
        };
        let backlog = self.stats.max_backlog_since_log;
        let no_slot = self.stats.skipped_no_slot;
        let not_ready = self.stats.skipped_not_ready;
        self.stats.max_backlog_since_log = 0;
        self.stats.skipped_no_slot = 0;
        self.stats.skipped_not_ready = 0;
        eprintln!(
            "terminal: {fps:.1} fps | present-busy {busy_us:.0} us/frame \
             (interval {interval_us:.0} us) -> {bound} | backlog max {backlog} | \
             skipped no-slot {no_slot} not-ready {not_ready}"
        );
    }

    /// On a slow present (>5ms -- never the 60fps steady state, always the
    /// spikes we're chasing), break the timed region into its three phases so
    /// we can see WHICH one owns the ~16ms. Gated behind `DebugFps::Log` so
    /// it's free otherwise. `submit_us` includes the stale-row shm memcpy plus
    /// the Attach/Damage/Commit ring writes -- if the socket is applying
    /// backpressure (compositor can't drain), the write blocks there and this
    /// phase, not blit, will own the time.
    pub(crate) fn log_slow_present(
        &self,
        total: std::time::Duration,
        t_blit: std::time::Duration,
        t_overlay: std::time::Duration,
        folded_len: usize,
        painted_rows: usize,
        painted_cells: usize,
    ) {
        if self.mode == DebugFps::Log && total.as_micros() > 5_000 {
            let blit_us = t_blit.as_micros();
            let overlay_us = t_overlay.saturating_sub(t_blit).as_micros();
            let submit_us = total.saturating_sub(t_overlay).as_micros();
            eprintln!(
                "terminal:   slow present {}us = blit {blit_us}us ({folded_len} folded, \
                 {painted_rows} rows / {painted_cells} cells) \
                 + overlay {overlay_us}us + shm-copy/submit {submit_us}us",
                total.as_micros()
            );
        }
    }
}

/// Rolling window of present timestamps, used to compute a live "presented
/// FPS" figure -- how often `TermWindow::maybe_present` actually flips a
/// frame, which (since presents are gated on the compositor's
/// `wl_callback.done`) is the true on-screen refresh rate, not just how busy
/// the render loop is.
struct FrameStats {
    /// Timestamps of the last up-to-`CAP` presents, oldest first.
    times: std::collections::VecDeque<std::time::Instant>,
    /// Wall-clock instant `times` was last logged to stderr (only used by
    /// [`DebugFps::Log`]).
    last_log: std::time::Instant,
    /// Accumulated CPU time spent doing the actual work inside
    /// `maybe_present` (blit + shm copy + damage bookkeeping) since the last
    /// log, and how many presents that covers. Divided out at log time to
    /// answer the only question that matters under a flood: is a low fps
    /// CPU-bound (busy ~ frame interval) or compositor-bound (busy <<
    /// interval, i.e. we're blocked waiting on `wl_callback.done`)?
    busy_since_log: std::time::Duration,
    presents_since_log: u32,
    /// Largest `pending_dirty` backlog (snapshots awaiting one present) seen
    /// since the last log. A monotonic ramp here is the signature of the
    /// present being gated while the parser keeps queueing frames.
    max_backlog_since_log: usize,
    /// Times `maybe_present` bailed because the compositor still held both shm
    /// buffers (`wl_buffer.release` lagging), vs. because we hadn't yet gotten
    /// the frame callback. Distinguishes buffer-starvation from vsync pacing.
    skipped_no_slot: u32,
    skipped_not_ready: u32,
}

/// How many recent presents [`FrameStats`] keeps around to average over.
const FRAME_STATS_CAP: usize = 120;

/// Presents older than this (relative to the newest present) are dropped
/// from the stats window. Without an age bound the average spans idle gaps:
/// while scrolled into history the cursor is hidden, so the blink heartbeat
/// stops presenting, and every pause between wheel detents stays inside the
/// 120-present window — the readout sagged to ~40 and then froze there while
/// idling in scrollback, even though each presented frame was full-rate. An
/// age bound makes `fps()` mean "presents per second over the last second of
/// activity", which is what the overlay claims to show.
const FRAME_STATS_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(1);

impl FrameStats {
    fn new() -> Self {
        Self {
            times: std::collections::VecDeque::with_capacity(FRAME_STATS_CAP),
            last_log: std::time::Instant::now(),
            busy_since_log: std::time::Duration::ZERO,
            presents_since_log: 0,
            max_backlog_since_log: 0,
            skipped_no_slot: 0,
            skipped_not_ready: 0,
        }
    }

    /// Drop presents older than [`FRAME_STATS_MAX_AGE`] relative to `now`.
    /// Called both when a present is recorded AND before every read of
    /// [`Self::fps`] (overlay/log), so an idle stretch (no presents while
    /// parked in scrollback) decays the reading to 0 instead of freezing the
    /// last sagged average on screen forever.
    fn prune(&mut self, now: std::time::Instant) {
        while self
            .times
            .front()
            .is_some_and(|&t| now.duration_since(t) > FRAME_STATS_MAX_AGE)
        {
            self.times.pop_front();
        }
    }

    fn note_backlog(&mut self, depth: usize) {
        self.max_backlog_since_log = self.max_backlog_since_log.max(depth);
    }

    /// Add the CPU time one `maybe_present` spent on real work (not blocked on
    /// the compositor).
    fn record_busy(&mut self, dur: std::time::Duration) {
        self.busy_since_log += dur;
        self.presents_since_log += 1;
    }

    /// Mean busy microseconds per present since the last log, then reset the
    /// accumulator. `0.0` if no presents happened.
    ///
    /// This is a diagnostics-only average (logged frame-pacing stats); a
    /// `u128` microsecond count losing precision in the f64 conversion at
    /// any value this process could plausibly accumulate is not observable
    /// in the logged output.
    #[allow(clippy::cast_precision_loss)]
    fn drain_busy_us(&mut self) -> f64 {
        let n = self.presents_since_log;
        let total = std::mem::take(&mut self.busy_since_log);
        self.presents_since_log = 0;
        if n == 0 {
            0.0
        } else {
            total.as_micros() as f64 / f64::from(n)
        }
    }

    /// Record a present that just happened.
    fn record(&mut self) {
        let now = std::time::Instant::now();
        self.times.push_back(now);
        while self.times.len() > FRAME_STATS_CAP {
            self.times.pop_front();
        }
        // Age-bound the window so idle gaps (no presents while parked in
        // scrollback — the hidden cursor stops the blink heartbeat) don't
        // get averaged into the next reading as phantom slowness.
        self.prune(now);
    }

    /// Presented frames per second, averaged over the retained window.
    /// `0.0` until at least two presents have been recorded.
    fn fps(&self) -> f32 {
        let Some(&first) = self.times.front() else {
            return 0.0;
        };
        let Some(&last) = self.times.back() else {
            return 0.0;
        };
        let span = last.duration_since(first).as_secs_f32();
        if span <= 0.0 || self.times.len() < 2 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)] // FRAME_STATS_CAP is tiny (120)
        let intervals = (self.times.len() - 1) as f32;
        intervals / span
    }
}
