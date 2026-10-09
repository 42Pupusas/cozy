//! FPS/frame-pacing debug-instrumentation dispatch for [`TermWindow`].
//!
//! Split out of `window.rs` into its own file, and — unlike the six
//! separately-`#[cfg]`'d method pairs this replaced — gated as two whole
//! `impl` blocks: one real, one all-no-op twin. Per-method `#[cfg]` scattered
//! through `window.rs` read as "business logic with debug branches sprinkled
//! in"; two block-level impls read as what this actually is, a single
//! optional subsystem with a real implementation and a zero-cost stand-in,
//! which is the shape our `#[cfg(feature = ...)]` convention asks for.
//!
//! Every `debug_*` method still takes `&mut self`/`&self` matching its real
//! twin exactly (even where the no-op body doesn't need it) because call
//! sites in `window.rs` (`self.debug_pre_present()`, etc.) must compile
//! unchanged under either `cfg` — the resulting `clippy::unused_self` on the
//! no-op twins is structural to the pattern, not fixable per-method without
//! forcing every call site to branch on the feature itself.

use super::TermWindow;

#[cfg(feature = "trace")]
impl TermWindow {
    /// Note the current `pending_dirty` backlog depth and emit the
    /// once-a-second frame-pacing log line.
    pub(super) fn debug_pre_present(&mut self) {
        self.debug.note_backlog(self.pending_dirty.len());
        self.debug.log_frame_pacing();
    }

    /// Record that a present was skipped because the frame callback hadn't
    /// arrived yet (vsync pacing).
    pub(super) const fn debug_note_skipped_not_ready(&mut self) {
        self.debug.note_skipped_not_ready();
    }

    /// Record that a present was skipped because no shm buffer slot was free
    /// (buffer starvation).
    pub(super) const fn debug_note_skipped_no_slot(&mut self) {
        self.debug.note_skipped_no_slot();
    }

    /// Remap the fps overlay's dirty-row tracker by a signed whole-screen
    /// shift (a viewport pixel-scroll).
    pub(super) fn debug_shift_prev_rows_signed(&mut self, shifts: &[i32], row_count: u16) {
        Self::shift_overlay_rows_signed(&mut self.debug.prev_rows, shifts, row_count);
    }

    /// Remap the fps overlay's dirty-row tracker by an upward scroll band.
    pub(super) fn debug_shift_prev_rows(&mut self, scrolls: &[grid::ScrollBand]) {
        Self::shift_overlay_rows(&mut self.debug.prev_rows, scrolls);
    }

    /// Take the fps overlay's previously-touched rows for this frame's
    /// repaint-clean pass.
    pub(super) fn debug_take_prev_rows(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.debug.prev_rows)
    }

    /// Record this present in the rolling fps average and draw the "NN.N
    /// fps" overlay text, returning the rows it touched.
    pub(super) fn debug_draw_overlay(&mut self) -> Vec<u16> {
        self.debug.record_present();
        let rows = self.debug.draw(
            self.present.slot_pixels(self.paint_slot),
            self.width,
            self.height,
            self.cell_w,
            &mut self.renderer,
        );
        self.debug.prev_rows.clone_from(&rows);
        rows
    }

    /// Record this present's busy time and, on a slow present, log the phase
    /// breakdown.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn debug_record_busy_and_log(
        &mut self,
        total: std::time::Duration,
        t_snapshot: std::time::Duration,
        t_paint: std::time::Duration,
        t_blit: std::time::Duration,
        t_overlay: std::time::Duration,
        folded_len: usize,
        painted_rows: usize,
        painted_cells: usize,
    ) {
        self.debug.record_busy(total);
        self.debug.log_slow_present(
            total,
            t_snapshot,
            t_paint,
            t_blit,
            t_overlay,
            folded_len,
            painted_rows,
            painted_cells,
        );
    }
}

/// The `not(trace)` twin: every hook above compiled down to a free no-op, so
/// a release build carries zero extra state and zero extra branches for
/// this instrumentation. Signatures deliberately mirror the real impl
/// exactly (see the module doc) so call sites need no `cfg` of their own.
#[cfg(not(feature = "trace"))]
#[allow(clippy::unused_self, clippy::too_many_arguments)]
impl TermWindow {
    pub(super) const fn debug_pre_present(&self) {}

    pub(super) const fn debug_note_skipped_not_ready(&self) {}

    pub(super) const fn debug_note_skipped_no_slot(&self) {}

    pub(super) const fn debug_shift_prev_rows_signed(&self, _shifts: &[i32], _row_count: u16) {}

    pub(super) const fn debug_shift_prev_rows(&self, _scrolls: &[grid::ScrollBand]) {}

    pub(super) const fn debug_take_prev_rows(&self) -> Vec<u16> {
        Vec::new()
    }

    pub(super) const fn debug_draw_overlay(&self) -> Vec<u16> {
        Vec::new()
    }

    pub(super) const fn debug_record_busy_and_log(
        &self,
        _total: std::time::Duration,
        _t_snapshot: std::time::Duration,
        _t_paint: std::time::Duration,
        _t_blit: std::time::Duration,
        _t_overlay: std::time::Duration,
        _folded_len: usize,
        _painted_rows: usize,
        _painted_cells: usize,
    ) {
    }
}
