//! Frame-presentation state shared by every shm-backed window.
//!
//! The double-buffered `wl_shm` swap chain (two [`Frame`]s + the pool + pixel
//! format backing them), the per-slot bitmap of rows each slot lags behind
//! the latest image, and the dirty/frame-ready/frame-callback bookkeeping
//! that decides *when* a present may happen.
//!
//! Windows paint straight into the claimed slot; there is no backbuffer.
//! A window that redraws every pixel uses [`PresentState::repaint_slot`]. A
//! window that paints incrementally calls [`PresentState::catch_up`] first,
//! which copies the rows the slot lags from the slot presented last, and
//! [`PresentState::publish_rows`] after, so the other slot knows what it now
//! lags.

use crate::conn::WindowSender;
use crate::frame_carry::FrameCarry;
use crate::id::{BufferId, CallbackId, PoolId};
use crate::protocol::{BufferRequest, Format, ShmPoolRequest};
use crate::row_band::RowBand;
use crate::surface::{Frame, Routed};
use crate::sys::{ShmBuffer, SysError};
use crate::wire::MessageBuilder;

/// Outcome of [`PresentState::begin_present`].
///
/// Either a free slot is ready to present into, or one of the two reasons a
/// present must be skipped this tick (kept distinct so callers can
/// log/count them separately, matching the two skip counters `DebugOverlay`
/// already tracks).
pub enum PresentAttempt {
    /// Nothing has changed since the last present; no work to do.
    NotDirty,
    /// Content changed, but the compositor hasn't released the previous
    /// frame's `wl_callback.done` yet (vsync pacing).
    NotReady,
    /// Content changed and we're pacing-ready, but both shm slots are still
    /// held by the compositor (buffer starvation).
    NoFreeSlot,
    /// Slot `usize` is free and marked busy; the caller should paint into it.
    Ready(usize),
}

/// The double-buffered shm swap chain plus the dirty/ready handshake that
/// gates [`super::TermWindow::maybe_present`].
pub struct PresentState {
    frames: [Frame; 2],
    shm_mem: ShmBuffer,
    /// The shm pool backing `frames`. Created once and reused across
    /// resizes: it grows in place (`ftruncate` + `mremap` +
    /// `wl_shm_pool.resize`) and shrinks lazily (the pool stays oversized).
    pool: PoolId,
    /// `wl_shm` pixel format backing `frames` -- `Argb8888` when the
    /// renderer has a window opacity < 1.0 configured, else the cheaper
    /// `Xrgb8888`. Fixed for the window's lifetime.
    buf_format: Format,
    /// Per-frame-slot "needs re-copying from the backbuffer" bitmap, indexed
    /// by cell row, index-parallel with `frames`. See the field doc this
    /// carried on `TermWindow` before extraction for the full rationale.
    stale_rows: [Vec<bool>; 2],
    /// Whether the backbuffer has changed since the last present.
    dirty: bool,
    /// Whether the compositor has released the previous frame callback,
    /// i.e. we're allowed to present again (vsync pacing).
    frame_ready: bool,
    /// The in-flight `wl_callback` we're waiting on, if any.
    frame_cb: Option<CallbackId>,
    /// The slot presented into last (i.e. the pool region the compositor is
    /// currently scanning out), if any. `begin_present` prefers the *other*
    /// slot so it never overwrites memory that's still on screen -- the
    /// source of the resize/drag "diagonal tear". `None` before the first
    /// present, when either slot is equally safe.
    last_slot: Option<usize>,
    /// The slot that held the latest image when the current present began,
    /// i.e. `last_slot` before [`Self::begin_present`] moved it.
    /// [`Self::catch_up`] copies from here.
    source: Option<usize>,
    full_damage: bool,
}

impl PresentState {
    /// Build the initial swap-chain state. `row_count` seeds both slots'
    /// stale bitmap fully true: both shm buffers start as blank/freshly
    /// mmap'd memory, so every row needs a first copy from the backbuffer
    /// before either slot shows the real image.
    #[must_use]
    pub fn new(
        frames: [Frame; 2],
        shm_mem: ShmBuffer,
        pool: PoolId,
        buf_format: Format,
        row_count: usize,
    ) -> Self {
        Self {
            frames,
            shm_mem,
            pool,
            buf_format,
            stale_rows: [vec![true; row_count.max(1)], vec![true; row_count.max(1)]],
            dirty: true,
            frame_ready: true,
            frame_cb: None,
            last_slot: None,
            source: None,
            full_damage: true,
        }
    }

    /// `wl_shm` pixel format this window's buffers use.
    #[must_use]
    pub const fn buf_format(&self) -> Format {
        self.buf_format
    }

    /// Flag that the backbuffer has changed and a present is owed.
    pub const fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Mark every row of both slots as lagging, so the next catch-up copies
    /// the whole latest image.
    pub fn mark_all_stale(&mut self) {
        for slot in &mut self.stale_rows {
            for s in slot.iter_mut() {
                *s = true;
            }
        }
    }

    /// Route the connection-level events every window handles identically
    /// (`wl_buffer.release`, the frame callback) through
    /// [`crate::surface::SurfaceBootstrap::route_common`], borrowing exactly
    /// the fields it needs. Callers still handle `wl_display` events and
    /// role-specific dispatch themselves via the returned [`Routed`].
    pub fn route_common(
        &mut self,
        ev: &crate::event::RawEvent,
        window_tx: &mut WindowSender,
    ) -> Routed {
        crate::surface::SurfaceBootstrap::route_common(
            ev,
            window_tx,
            &mut self.frames,
            &mut self.frame_ready,
            &mut self.frame_cb,
        )
    }

    /// Decide whether a present may happen this tick, and if so claim a free
    /// slot (marking it busy). See [`PresentAttempt`] for the possible
    /// outcomes.
    pub fn begin_present(&mut self) -> PresentAttempt {
        if !self.dirty {
            return PresentAttempt::NotDirty;
        }
        if !self.frame_ready {
            return PresentAttempt::NotReady;
        }
        // Ping-pong: prefer the slot we did NOT present into last time. The
        // last-presented slot's pool memory is what the compositor is still
        // scanning out, so reusing it -- which the old "first free slot"
        // choice did on every resize (`recreate_buffers` force-frees both
        // slots, making slot 0 always win) and whenever releases were prompt
        // -- overwrites on-screen pixels mid-scanout and tears the frame
        // diagonally. Only fall back to the last slot if its sibling is still
        // held by the compositor.
        let sibling = self.last_slot.map(|s| 1 - s);
        let slot = sibling
            .filter(|&s| self.frames.get(s).is_some_and(|f| f.free))
            .or_else(|| self.frames.iter().position(|f| f.free));
        let Some(slot) = slot else {
            return PresentAttempt::NoFreeSlot;
        };
        self.frames[slot].free = false;
        self.source = self.last_slot;
        self.last_slot = Some(slot);
        PresentAttempt::Ready(slot)
    }

    /// Slot `slot`'s shm pixels: the buffer the compositor reads once this
    /// present commits. Windows paint here directly; there is no separate
    /// backbuffer.
    pub fn slot_pixels(&mut self, slot: usize) -> &mut [u32] {
        self.shm_mem.pixels(slot)
    }

    /// Slot `slot`'s shm pixels as a `width` × `height` canvas.
    pub fn slot_canvas(&mut self, slot: usize, width: i32, height: i32) -> crate::draw::Canvas<'_> {
        crate::draw::Canvas::new(self.shm_mem.pixels(slot), width, height)
    }

    /// Clear `slot` to `bg` and run `paint` over it: the whole present for a
    /// window that redraws every pixel each frame.
    pub fn repaint_slot(
        &mut self,
        slot: usize,
        size: (i32, i32),
        format: crate::SurfaceFormat,
        bg: u32,
        paint: &mut impl FnMut(&mut crate::draw::Canvas<'_>),
    ) {
        let pixels = self.shm_mem.pixels(slot);
        pixels.fill(bg);
        let mut canvas = crate::draw::Canvas::new(pixels, size.0, size.1).with_format(format);
        paint(&mut canvas);
    }

    /// Bring `slot` level with the latest presented image before an
    /// incremental paint, by copying the rows it lags from the slot that was
    /// presented last. Windows that repaint every pixel each present skip
    /// this.
    pub fn catch_up(&mut self, slot: usize, band: RowBand) {
        let mut stale = std::mem::take(&mut self.stale_rows[slot]);
        if let Some(source) = self.source.filter(|&s| s != slot) {
            for lines in band.runs(&stale) {
                self.shm_mem.copy_between(source, slot, band.pixels(&lines));
            }
        }
        stale.fill(false);
        self.stale_rows[slot] = stale;
    }

    /// Record the rows painted into `slot` this present: every other slot
    /// now lags the latest image there.
    pub fn publish_rows(&mut self, slot: usize, rows: &[u16]) {
        for (index, stale) in self.stale_rows.iter_mut().enumerate() {
            if index == slot {
                continue;
            }
            for &row in rows {
                if let Some(flag) = stale.get_mut(usize::from(row)) {
                    *flag = true;
                }
            }
        }
    }

    /// Whether this present must damage the whole surface: the first one, and
    /// the first after a resize, have no previous frame to diff against.
    pub const fn take_full_damage(&mut self) -> bool {
        let full = self.full_damage;
        self.full_damage = false;
        full
    }

    /// Fill every slot with `value`, so a slot presented before it is
    /// painted shows the background instead of zeroed memory.
    pub fn fill_all(&mut self, value: u32) {
        for slot in 0..self.frames.len() {
            self.shm_mem.pixels(slot).fill(value);
        }
    }

    /// Fill scanlines `line..` of every slot with `value`.
    pub fn fill_lines_from(&mut self, band: RowBand, line: usize, value: u32) {
        let pixels = band.pixels(&(line..band.height()));
        for slot in 0..self.frames.len() {
            if let Some(strip) = self.shm_mem.pixels(slot).get_mut(pixels.clone()) {
                strip.fill(value);
            }
        }
    }

    /// A copy of the latest presented image, taken before a resize reshapes
    /// the pool so it can be painted back into the new slots.
    #[must_use]
    pub fn capture_latest(&mut self, width: i32, height: i32) -> FrameCarry {
        let slot = self.last_slot.unwrap_or(0);
        FrameCarry::capture(self.shm_mem.pixels(slot), width, height)
    }

    /// Paint `carry` into every slot at the new `width` × `height`, filling
    /// the uncovered area with `bg`. Every slot then holds the same image, so
    /// none lags.
    pub fn restore(&mut self, carry: &FrameCarry, width: i32, height: i32, bg: u32) {
        for slot in 0..self.frames.len() {
            carry.paint_onto(self.shm_mem.pixels(slot), width, height, bg);
        }
        for stale in &mut self.stale_rows {
            stale.fill(false);
        }
    }

    /// Read-only peek at slot `slot`'s stale-row bitmap, for tests that
    /// don't need the shm pixels alongside it.
    #[cfg(test)]
    fn stale_rows_for_test(&self, slot: usize) -> &[bool] {
        &self.stale_rows[slot]
    }

    /// The `wl_buffer` object backing slot `slot`, for `Attach`.
    #[must_use]
    pub const fn frame_buffer(&self, slot: usize) -> BufferId {
        self.frames[slot].buffer
    }

    /// Record that a present just went out: arm the frame-callback wait and
    /// clear dirty/ready until the compositor answers.
    pub const fn finish_present(&mut self, cb: CallbackId) {
        self.frame_cb = Some(cb);
        self.frame_ready = false;
        self.dirty = false;
    }

    /// Destroy the old `wl_buffer`s ahead of a resize. The pool itself is
    /// reused — only the per-resize buffers are torn down and recreated.
    pub fn destroy_old_buffers(&self, window_tx: &mut WindowSender) {
        for frame in &self.frames {
            window_tx.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy));
        }
    }

    /// Re-shape the backing shm mapping for a `w x h` buffer. Returns
    /// `Some(new_capacity)` if the pool had to grow (caller must then issue
    /// [`Self::grow_pool`] before recreating buffers), or `None` on a lazy
    /// shrink (no server round-trip needed).
    pub fn resize_shm(&mut self, w: i32, h: i32) -> Result<Option<usize>, SysError> {
        // Window dimensions are always non-negative in practice (validated
        // config / compositor-reported sizes); clamp defensively rather
        // than panic on a pathological value.
        self.shm_mem.resize(
            usize::try_from(w).unwrap_or(0),
            usize::try_from(h).unwrap_or(0),
        )
    }

    /// Release both buffers and the pool, for a surface that is going away
    /// while the connection lives on.
    pub fn destroy(&self, window_tx: &WindowSender) {
        for frame in &self.frames {
            window_tx.request(MessageBuilder::new(frame.buffer, BufferRequest::Destroy));
        }
        window_tx.request(MessageBuilder::new(self.pool, ShmPoolRequest::Destroy));
    }

    /// Tell the compositor the pool grew, ahead of carving new buffers out
    /// of the extra space.
    pub fn grow_pool(&self, window_tx: &mut WindowSender, new_capacity: usize) {
        window_tx.request(
            MessageBuilder::new(self.pool, ShmPoolRequest::Resize)
                .i32(i32::try_from(new_capacity).unwrap_or(i32::MAX)),
        );
    }

    /// Carve fresh `w x h` buffers for both slots out of the (possibly just
    /// grown) pool, and mark both free.
    pub fn recreate_buffers(&mut self, window_tx: &mut WindowSender, w: i32, h: i32) {
        let stride = w * 4;
        for (i, frame) in self.frames.iter_mut().enumerate() {
            let buffer: BufferId = window_tx.alloc_id();
            window_tx.request(
                MessageBuilder::new(self.pool, ShmPoolRequest::CreateBuffer)
                    .new_id(buffer)
                    .i32(i32::try_from(self.shm_mem.frame_offset(i)).unwrap_or(i32::MAX))
                    .i32(w)
                    .i32(h)
                    .i32(stride)
                    .u32(self.buf_format.code()),
            );
            frame.buffer = buffer;
            frame.free = true;
        }
    }

    /// After a resize, both shm buffers are brand-new (freshly created,
    /// uninitialized) memory: every row is stale relative to the backbuffer
    /// in both slots, and any in-flight frame callback referred to the
    /// now-destroyed surface contents. Force the next present to fire
    /// immediately (never leave the surface showing a detached/blank
    /// buffer — the cause of resize flashing).
    pub fn reset_after_resize(&mut self, row_count: usize) {
        self.dirty = true;
        self.full_damage = true;
        self.stale_rows = [vec![true; row_count.max(1)], vec![true; row_count.max(1)]];
        self.frame_ready = true;
        self.frame_cb = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sender() -> WindowSender {
        use quetzalcoatl::capacity::Capacity;
        use quetzalcoatl::mpsc;
        let (tx, _rx) =
            mpsc::RingBuffer::<crate::conn::WindowRequest>::new(Capacity::exact(64)).split();
        WindowSender {
            tx,
            next_id: 100,
            id_end: 200,
            free_ids: Vec::new(),
        }
    }

    fn make_state() -> PresentState {
        let frames = [
            Frame {
                buffer: BufferId(1),
                free: true,
            },
            Frame {
                buffer: BufferId(2),
                free: true,
            },
        ];
        let shm_mem = ShmBuffer::new(4, 4, 2).expect("alloc");
        PresentState::new(frames, shm_mem, PoolId(9), Format::Xrgb8888, 4)
    }

    #[test]
    fn new_state_starts_dirty_and_ready() {
        let mut st = make_state();
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
    }

    #[test]
    fn begin_present_not_dirty_after_finish() {
        let mut st = make_state();
        let cb = CallbackId(42);
        st.finish_present(cb);
        assert!(matches!(st.begin_present(), PresentAttempt::NotDirty));
    }

    #[test]
    fn begin_present_not_ready_when_dirty_but_frame_not_ready() {
        let mut st = make_state();
        st.finish_present(CallbackId(1));
        st.mark_dirty();
        assert!(matches!(st.begin_present(), PresentAttempt::NotReady));
    }

    #[test]
    fn begin_present_no_free_slot_when_both_busy() {
        let mut st = make_state();
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(1)));
        st.mark_dirty();
        assert!(matches!(st.begin_present(), PresentAttempt::NoFreeSlot));
    }

    #[test]
    fn publish_rows_marks_only_the_other_slot() {
        let mut st = make_state();
        st.stale_rows[0].fill(false);
        st.stale_rows[1].fill(false);
        st.publish_rows(0, &[1]);
        assert!(!st.stale_rows_for_test(0)[1]);
        assert!(st.stale_rows_for_test(1)[1]);
        assert!(!st.stale_rows_for_test(1)[0]);
    }

    #[test]
    fn mark_all_stale_sets_every_row_both_slots() {
        let mut st = make_state();
        st.stale_rows[0].iter_mut().for_each(|s| *s = false);
        st.stale_rows[1].iter_mut().for_each(|s| *s = false);
        st.mark_all_stale();
        assert!(st.stale_rows_for_test(0).iter().all(|&s| s));
        assert!(st.stale_rows_for_test(1).iter().all(|&s| s));
    }

    #[test]
    fn reset_after_resize_keeps_ping_pong_history() {
        // After a resize the on-screen buffer is still the last-presented
        // slot; `reset_after_resize` must not forget it, or the immediate
        // post-resize present could reuse the scanned-out region and tear.
        let mut st = make_state();
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        st.frames[0].free = true;
        st.frames[1].free = true;
        st.reset_after_resize(4);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(1)));
    }

    #[test]
    fn reset_after_resize_forces_ready_and_dirty() {
        let mut st = make_state();
        st.finish_present(CallbackId(1));
        assert!(matches!(st.begin_present(), PresentAttempt::NotDirty));
        st.reset_after_resize(4);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(_)));
        assert!(st.stale_rows_for_test(0).iter().all(|&s| s));
    }

    #[test]
    fn begin_present_ping_pongs_between_slots() {
        let mut st = make_state();
        // First present: no history, takes the first free slot (0).
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        // Compositor releases slot 0 and answers the frame callback.
        st.frames[0].free = true;
        st.finish_present(CallbackId(1));
        st.frame_ready = true;
        st.mark_dirty();
        // Even though slot 0 is free again, we must NOT reuse it -- it's the
        // one on screen. Ping-pong to slot 1.
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(1)));
        st.frames[1].free = true;
        st.finish_present(CallbackId(2));
        st.frame_ready = true;
        st.mark_dirty();
        // And back to slot 0.
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
    }

    #[test]
    fn begin_present_falls_back_to_last_slot_when_sibling_busy() {
        let mut st = make_state();
        // Present into slot 0, leave it free, but keep slot 1 held busy.
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        st.frames[0].free = true;
        st.frames[1].free = false;
        st.finish_present(CallbackId(1));
        st.frame_ready = true;
        st.mark_dirty();
        // Sibling (1) is busy, so we have no choice but to reuse slot 0.
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
    }

    fn release_and_ready(st: &mut PresentState, slot: usize) {
        st.frames[slot].free = true;
        st.finish_present(CallbackId(7));
        st.frame_ready = true;
        st.mark_dirty();
    }

    #[test]
    fn catch_up_copies_the_rows_painted_into_the_other_slot() {
        let mut st = make_state();
        let band = RowBand::new(4, 4, 1);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        st.catch_up(0, band);
        st.slot_pixels(0)[4..8].fill(5);
        st.publish_rows(0, &[1]);
        release_and_ready(&mut st, 0);

        assert!(matches!(st.begin_present(), PresentAttempt::Ready(1)));
        st.catch_up(1, band);
        assert_eq!(&st.slot_pixels(1)[4..8], &[5; 4]);
        assert_eq!(&st.slot_pixels(1)[0..4], &[0; 4]);
    }

    #[test]
    fn catch_up_only_copies_rows_the_slot_lags() {
        let mut st = make_state();
        let band = RowBand::new(4, 4, 1);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        st.catch_up(0, band);
        release_and_ready(&mut st, 0);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(1)));
        st.catch_up(1, band);
        st.slot_pixels(1)[0..4].fill(9);
        st.publish_rows(1, &[0]);
        release_and_ready(&mut st, 1);

        st.slot_pixels(0)[12..16].fill(3);
        assert!(matches!(st.begin_present(), PresentAttempt::Ready(0)));
        st.catch_up(0, band);
        assert_eq!(&st.slot_pixels(0)[0..4], &[9; 4]);
        assert_eq!(&st.slot_pixels(0)[12..16], &[3; 4]);
    }

    #[test]
    fn the_first_present_and_the_first_after_a_resize_damage_everything() {
        let mut st = make_state();
        assert!(st.take_full_damage());
        assert!(!st.take_full_damage());
        st.reset_after_resize(4);
        assert!(st.take_full_damage());
    }

    #[test]
    fn restore_paints_every_slot_and_clears_lag() {
        let mut st = make_state();
        st.slot_pixels(0).copy_from_slice(&[1; 16]);
        let carry = st.capture_latest(4, 4);
        st.restore(&carry, 4, 4, 0);
        assert_eq!(st.slot_pixels(1), &[1; 16]);
        assert!(st.stale_rows_for_test(0).iter().all(|&s| !s));
        assert!(st.stale_rows_for_test(1).iter().all(|&s| !s));
    }

    #[test]
    fn repaint_slot_clears_then_paints() {
        let mut st = make_state();
        st.slot_pixels(1).fill(8);
        st.repaint_slot(
            1,
            (4, 4),
            crate::SurfaceFormat::opaque(),
            2,
            &mut |canvas| {
                canvas.pixels_mut()[0] = 6;
            },
        );
        let pixels = st.slot_pixels(1);
        assert_eq!(pixels[0], 6);
        assert!(pixels[1..].iter().all(|&p| p == 2));
    }

    #[test]
    fn frame_buffer_reports_the_right_slot() {
        let st = make_state();
        assert_eq!(st.frame_buffer(0), BufferId(1));
        assert_eq!(st.frame_buffer(1), BufferId(2));
    }

    #[test]
    fn destroy_old_buffers_does_not_panic_without_a_live_connection() {
        let st = make_state();
        let mut tx = test_sender();
        st.destroy_old_buffers(&mut tx);
    }
}
