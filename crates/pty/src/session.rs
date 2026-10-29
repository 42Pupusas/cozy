//! The running event loop: the `io_uring` and the buffers its operations
//! read into and write from.
//!
//! A [`Session`] is created by [`Pty::run`] on the loop's own thread and
//! never leaves it. `IoUring` is not `Send`, so the compiler holds the ring
//! to the thread that created it, which `DEFER_TASKRUN` requires.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use ququmatz::{CqeFlags, IoUring, IoUringBuilder, PollMask, ProvidedBufferRing, Sqe};

use crate::action::CqeAction;
use crate::inflight::{InFlight, OpTag};
use crate::outcome::Outcome;
use crate::spawned::Spawned;
use crate::stats::Stats;
use crate::write_queue::WriteQueue;
use crate::{EAGAIN, EINTR, PtyError, PtyInput, READ_BUF_SIZE, WinSize};

/// Number of `io_uring` entries for the PTY ring.
const RING_ENTRIES: u32 = 64;

/// Buffers in the pool the multishot read fills. With a rejected chunk
/// holding its buffer, this bounds how much output is read ahead of the
/// consumer.
const READ_POOL_BUFFERS: u32 = 16;

/// Buffer group id of the read pool; the ring has no other group.
const READ_GROUP: u16 = 0;

/// `user_data` for the cancel SQEs submitted during teardown. Outside the
/// range [`OpTag`] decodes, so a cancel's own completion is never mistaken
/// for an operation completing.
const CANCEL_USER_DATA: u64 = 0xffff_0001;

pub struct Session {
    /// The ring driving all I/O. Declared first so it closes before the
    /// buffers below are freed.
    ///
    /// Field order alone is **not** sufficient for soundness. Closing the
    /// ring fd starts the kernel's teardown but does not wait for it, so a
    /// read armed against `read_buf` can complete after that buffer is
    /// freed. [`Session::quiesce`] cancels the outstanding operations and
    /// waits for their completions before anything is dropped.
    ring: IoUring,
    /// Buffers the multishot master-fd read fills, one chunk per CQE.
    pool: ProvidedBufferRing,
    pty: Spawned,
    /// Input waiting for the master fd. Writes are serialized — one SQE at
    /// a time — because concurrent writes to the same fd carry no ordering
    /// guarantee and terminal input bytes must stay strictly ordered.
    writes: WriteQueue,
    /// Stable buffer the kernel writes `signalfd_siginfo` into.
    signal_buf: Box<[u8; 128]>,
    /// Stable 8-byte buffer for the eventfd counter read. Reading the
    /// counter also resets it, so the read SQE doubles as the drain.
    wake_buf: Box<[u8; 8]>,
    /// Chunks `on_output` rejected (consumer ring full), as `(buffer id,
    /// length)` in arrival order. Each keeps its pool buffer until accepted,
    /// and the read is not re-armed while any are queued. Once the pool runs
    /// dry the kernel stops the read, the pty output buffer fills, and the
    /// child's `write(2)` blocks: backpressure instead of dropped bytes.
    unread: VecDeque<(u16, u32)>,
    /// Which submitted operations the kernel has not completed yet, and so
    /// whose buffers it may still write into. Teardown must not free a
    /// buffer while its operation is in this set.
    in_flight: InFlight,
}

impl Session {
    /// Create the ring on the calling thread. On failure the child is hung
    /// up and the pty's fds are closed, since no loop will ever run them.
    pub fn new(pty: Spawned) -> Result<Self, PtyError> {
        let (ring, pool) = match Self::setup() {
            Ok(parts) => parts,
            Err(err) => {
                pty.teardown(false);
                return Err(PtyError::UringSetup(err));
            }
        };
        Ok(Self {
            ring,
            pool,
            pty,
            writes: WriteQueue::default(),
            signal_buf: Box::new([0u8; 128]),
            wake_buf: Box::new([0u8; 8]),
            unread: VecDeque::new(),
            in_flight: InFlight::new(),
        })
    }

    fn setup() -> Result<(IoUring, ProvidedBufferRing), ququmatz::Error> {
        let mut ring = IoUringBuilder::new(RING_ENTRIES)
            .single_issuer()
            .defer_taskrun()
            .build()?;
        let buf_size = u32::try_from(READ_BUF_SIZE).expect("READ_BUF_SIZE fits in u32");
        let pool = ring.register_provided_buffers(READ_GROUP, READ_POOL_BUFFERS, buf_size)?;
        Ok((ring, pool))
    }

    /// The loop behind [`Pty::run`](crate::Pty::run); see there for the
    /// contract.
    pub fn run<I, O>(mut self, mut next_input: I, mut on_output: O) -> Result<i32, PtyError>
    where
        I: FnMut() -> Option<PtyInput>,
        O: FnMut(&[u8]) -> bool,
    {
        let mut outcome = Outcome::default();
        let looped = self.event_loop(&mut outcome, &mut next_input, &mut on_output);
        outcome.reap_after_eof(self.pty.child_pid);

        // Before anything is freed: take the buffers back from the kernel.
        // This runs while the fds are still open, since cancelling an
        // operation needs the ring fd it was submitted to.
        let quiet = self.quiesce();

        self.pty.teardown(outcome.reaped);

        if !quiet {
            // The kernel never returned some completion, so it may still
            // write into these buffers. Leak them rather than hand the
            // allocator memory the kernel is using.
            std::mem::forget(self);
        }
        looped.map(|()| outcome.code)
    }

    fn event_loop<I, O>(
        &mut self,
        outcome: &mut Outcome,
        next_input: &mut I,
        on_output: &mut O,
    ) -> Result<(), PtyError>
    where
        I: FnMut() -> Option<PtyInput>,
        O: FnMut(&[u8]) -> bool,
    {
        self.arm_read()?;
        self.arm_signal()?;
        self.arm_wake()?;

        // Drain any input queued before the loop started (the producer may
        // have signalled the eventfd before our wake SQE was armed).
        let mut quit_requested = self.drain_input(next_input);
        self.flush_write()?;

        while !quit_requested && !self.pty.shutdown.load(Ordering::Acquire) {
            self.retry_output(on_output);
            if outcome.done && self.unread.is_empty() {
                break;
            }
            self.rearm_read()?;

            // Submit everything armed since the last lap — the arm_* and
            // flush_write helpers only queue — and park in the kernel until
            // at least one CQE is ready. While `unread` holds chunks the
            // read stays stopped once the pool runs dry, so this can only
            // wake on the signalfd or the wake eventfd — exactly the
            // throttle we want. The producer side must signal the wake
            // eventfd once it frees ring space so this doesn't stall.
            self.ring
                .submit_and_wait(1)
                .map_err(PtyError::UringEnter)?;

            for action in self.take_actions() {
                self.dispatch(action, outcome, on_output)?;
            }

            // Drain queued input once per wakeup (not just on Wake CQEs:
            // it's cheap and closes any drain-vs-signal race) and kick the
            // write path.
            quit_requested = self.drain_input(next_input);
            self.flush_write()?;
        }
        Ok(())
    }

    /// Chunks were rejected by the consumer ring — retry them before
    /// parking. Cheap, and closes the race where the wake signal that
    /// unblocked us arrived because the consumer just freed the space these
    /// very chunks need.
    fn retry_output<O: FnMut(&[u8]) -> bool>(&mut self, on_output: &mut O) {
        while let Some(&(buf_id, len)) = self.unread.front() {
            if !on_output(self.chunk(buf_id, len)) {
                break;
            }
            self.unread.pop_front();
            self.pool.recycle_and_commit(buf_id);
        }
    }

    /// Re-arm the multishot read if it stopped (pool ran dry, interrupted,
    /// or the kernel ended it) and no rejected chunk is waiting. Re-arming
    /// with chunks waiting would hit an empty pool and stop again at once.
    fn rearm_read(&mut self) -> Result<(), PtyError> {
        if self.unread.is_empty() && !self.in_flight.is_armed(OpTag::Read) {
            return self.arm_read();
        }
        Ok(())
    }

    fn chunk(&self, buf_id: u16, len: u32) -> &[u8] {
        self.pool.buffer(buf_id, len).unwrap_or(&[])
    }

    /// Drain the completion queue. Collected first because dispatch needs
    /// `self` mutably. Unbounded Vec: a burst of completions must never be
    /// truncated and lose a re-arm.
    fn take_actions(&mut self) -> Vec<CqeAction> {
        let mut actions = Vec::new();
        for cqe in self.ring.completions() {
            if let Some(tag) = OpTag::from_user_data(cqe.user_data()) {
                Stats::record(tag, cqe.raw_result());
                // Clear before dispatch: an action may re-arm the very tag
                // whose completion this batch carried. A multishot CQE
                // flagged MORE leaves its operation armed.
                if !cqe.flags().contains(CqeFlags::MORE) {
                    self.in_flight.complete(tag);
                }
            }
            actions.push(CqeAction::from_cqe(
                cqe.user_data(),
                cqe.raw_result(),
                cqe.buffer_id(),
            ));
        }
        actions
    }

    fn dispatch<O: FnMut(&[u8]) -> bool>(
        &mut self,
        action: CqeAction,
        outcome: &mut Outcome,
        on_output: &mut O,
    ) -> Result<(), PtyError> {
        match action {
            CqeAction::ReadOk { buf_id, len } => {
                // Behind a rejected chunk, queue to keep the bytes in order.
                if self.unread.is_empty() && on_output(self.chunk(buf_id, len)) {
                    self.pool.recycle_and_commit(buf_id);
                } else {
                    self.unread.push_back((buf_id, len));
                }
            }
            CqeAction::ReadEof => outcome.hit_eof(),
            CqeAction::WriteDone(result) => self.complete_write(result)?,
            CqeAction::WriteReady => self.submit_write(self.writes.retry())?,
            CqeAction::Signal => {
                if !outcome.child_signalled(self.pty.child_pid) {
                    self.arm_signal()?;
                }
            }
            // Re-arm FIRST, then drain: any signal that lands after the
            // drain finds the counter watched by an armed SQE, so no wakeup
            // is ever lost.
            CqeAction::Wake => self.arm_wake()?,
            CqeAction::None => {}
        }
        Ok(())
    }

    /// Pop everything `next_input` has, buffering bytes and coalescing
    /// resizes (only the last one is applied — an interactive drag fires a
    /// storm of configures but the shell only needs one SIGWINCH at the
    /// final size). Returns `true` if a [`PtyInput::Quit`] was seen.
    fn drain_input<I: FnMut() -> Option<PtyInput>>(&mut self, next_input: &mut I) -> bool {
        let mut pending_resize: Option<WinSize> = None;
        let mut quit = false;
        while let Some(msg) = next_input() {
            match msg {
                PtyInput::Bytes(b) => self.writes.push(b),
                PtyInput::Resize(ws) => pending_resize = Some(ws),
                PtyInput::Quit => quit = true,
            }
        }
        if let Some(ws) = pending_resize {
            let _ = self.pty.resize(ws);
        }
        quit
    }

    /// Submit the next write SQE if none is in flight and bytes are pending.
    fn flush_write(&mut self) -> Result<(), PtyError> {
        let next = self.writes.start();
        self.submit_write(next)
    }

    /// Submit a write for `range`, the front chunk's remainder from
    /// [`WriteQueue::start`] or [`WriteQueue::retry`]; `None` submits
    /// nothing.
    fn submit_write(&mut self, range: Option<(*const u8, u32)>) -> Result<(), PtyError> {
        let Some((ptr, len)) = range else {
            return Ok(());
        };
        // SAFETY: `ptr` points into the front chunk of `writes`, which stays
        // allocated and unpopped until this write's CQE is handled
        // (`WriteQueue::start`), and `quiesce` waits for that CQE before
        // the queue is dropped.
        let sqe = unsafe { Sqe::write_ptr(self.pty.master_fd, ptr, len, u64::MAX) };
        self.push(OpTag::Write, sqe)
    }

    /// Handle a write CQE. A short write leaves the rest of its chunk at the
    /// front of the queue.
    ///
    /// A negative result is a raw `-errno`. `EAGAIN` (the PTY line
    /// discipline's input queue is full, which a large burst write routinely
    /// hits) and `EINTR` mean *zero* bytes were written, so the same write
    /// goes again: after a `POLLOUT` for `EAGAIN`, at once for `EINTR`. Only
    /// a fatal errno (`EIO`/`EBADF`/etc — the child/slave side is actually
    /// gone) drops the chunk, since retrying that would spin forever.
    fn complete_write(&mut self, result: i32) -> Result<(), PtyError> {
        match result {
            r if r == -EAGAIN => self.arm_poll(OpTag::WritePoll, PollMask::OUT),
            r if r == -EINTR => self.submit_write(self.writes.retry()),
            // Matched non-negative, so the sign-preserving cast is exact.
            0.. => {
                self.writes.complete(Some(result.cast_unsigned() as usize));
                Ok(())
            }
            _ => {
                self.writes.complete(None);
                Ok(())
            }
        }
    }

    /// Wait for `mask` readiness on the master fd. The completion retries
    /// the write that returned `EAGAIN`.
    fn arm_poll(&mut self, tag: OpTag, mask: PollMask) -> Result<(), PtyError> {
        self.push(tag, Sqe::poll_add(self.pty.master_fd, mask))
    }

    /// One SQE that keeps reading the master fd into pool buffers, posting
    /// a CQE per chunk, until it stops. The kernel waits for data itself, so
    /// the read never sees `EAGAIN`.
    fn arm_read(&mut self) -> Result<(), PtyError> {
        let sqe = Sqe::read_multishot(self.pty.master_fd, u64::MAX, self.pool.bgid());
        self.push(OpTag::Read, sqe)
    }

    fn arm_signal(&mut self) -> Result<(), PtyError> {
        // SAFETY: signal_buf's heap allocation is owned by self, which
        // outlives every operation (see `quiesce`); only one signal read is
        // ever in flight against it.
        let sqe = unsafe {
            Sqe::read_ptr(self.pty.signal_fd, self.signal_buf.as_mut_ptr(), 128, u64::MAX)
        };
        self.push(OpTag::Signal, sqe)
    }

    fn arm_wake(&mut self) -> Result<(), PtyError> {
        // Reading an eventfd returns the counter AND resets it to zero, so
        // this armed read both wakes the loop and drains the count — several
        // coalesced signals cost one CQE.
        // SAFETY: wake_buf is a stable heap allocation owned by self; only
        // one wake read is ever in flight (re-armed after its CQE).
        let sqe =
            unsafe { Sqe::read_ptr(self.pty.wake_fd, self.wake_buf.as_mut_ptr(), 8, u64::MAX) };
        self.push(OpTag::Wake, sqe)
    }

    fn push(&mut self, tag: OpTag, sqe: Sqe) -> Result<(), PtyError> {
        self.ring
            .push(sqe.user_data(tag.as_user_data()))
            .map_err(|_| PtyError::SqFull)?;
        self.in_flight.arm(tag);
        Ok(())
    }

    /// Cancel every outstanding operation and wait for its completion, so
    /// the kernel owns none of our buffers when they are freed.
    ///
    /// Closing the ring fd is **not** enough on its own. The kernel's ring
    /// teardown is asynchronous, so a read armed against `read_buf` can
    /// still complete after that allocation has returned to the heap. The
    /// kernel then writes into a free chunk, and the corruption surfaces
    /// much later as an allocator abort in an unrelated thread (`malloc():
    /// unaligned tcache chunk`). The completion of an operation is the only
    /// proof that the kernel has let its buffer go.
    ///
    /// Waiting is safe while any operation is outstanding: cancelling a
    /// read, write or poll on a pollable fd always completes it.
    ///
    /// Returns `false` only when the wait itself fails. The caller then
    /// leaks the buffers, because freeing memory the kernel may still write
    /// into is not recoverable.
    fn quiesce(&mut self) -> bool {
        if self.in_flight.is_quiet() {
            return true;
        }
        if !self.cancel_outstanding() {
            return false;
        }
        loop {
            self.reap_completions();
            if self.in_flight.is_quiet() {
                return true;
            }
            match self.ring.submit_and_wait(1) {
                Ok(_) => {}
                Err(ququmatz::Error::Syscall(errno)) if errno.raw() == EINTR => {}
                Err(_) => return false,
            }
        }
    }

    /// Queue a cancel for each outstanding operation. Returns `false` if a
    /// full queue could not be submitted to make room.
    fn cancel_outstanding(&mut self) -> bool {
        for tag in self.in_flight.armed() {
            let sqe = Sqe::cancel(tag.as_user_data()).user_data(CANCEL_USER_DATA);
            while self.ring.push(sqe).is_err() {
                if self.ring.submit().is_err() {
                    return false;
                }
            }
        }
        true
    }

    fn reap_completions(&mut self) {
        for cqe in self.ring.completions() {
            if let Some(tag) = OpTag::from_user_data(cqe.user_data())
                && !cqe.flags().contains(CqeFlags::MORE)
            {
                self.in_flight.complete(tag);
            }
        }
    }
}
