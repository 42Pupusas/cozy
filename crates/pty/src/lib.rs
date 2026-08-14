//! Pseudoterminal management.
//!
//! Spawns a child process under a POSIX pseudoterminal and drives all I/O
//! through `io_uring`. The master fd is never touched by a blocking
//! `read(2)` / `write(2)` — every byte transfer goes through SQEs.
//!
//! # Architecture
//!
//! ```text
//!  forkpty()
//!    ├── child  → execs the shell (slave fd = stdin/stdout/stderr)
//!    └── parent → master fd registered with quetzal-uring UringQueue
//!                     │
//!                     ├── read SQE   (master fd)  → CQE → `on_output` callback
//!                     ├── write SQE  (master fd)  ← bytes from `next_input`
//!                     ├── read SQE   (signalfd)   → SIGCHLD → child reaped
//!                     └── read SQE   (wake eventfd) → drain `next_input`
//! ```
//!
//! [`Pty::run`] is the blocking event loop: it parks in `io_uring_enter`
//! until *any* of the four sources completes — no `poll(2)`, no
//! busy-spinning, no auxiliary wait primitive. Spawn it on a dedicated
//! thread.
//!
//! The caller owns the message transport: `run` pulls input via a
//! `next_input` closure and hands PTY output to an `on_output` closure, so
//! this crate stays agnostic of whichever ring buffers the application
//! wires between its threads. Producers must signal the wake eventfd
//! (passed to [`Pty::spawn`]) after queueing input; the armed eventfd read
//! SQE completes and the loop drains.
//!
//! [`PtySimple`] (plain non-blocking syscalls) is retained for tests and as
//! a reference implementation.

mod error;
mod inflight;
mod simple;
mod sys;

pub use error::PtyError;
pub use simple::PtySimple;
pub use sys::{ChildPid, CloseFd, PtyIoctl, RawPty, WaitStatus, WinSize};

use quetzal_uring::{CqConsumer, SqProducer, UringQueue};
use ququmatz::Sqe;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Number of `io_uring` entries for the PTY queue.
const RING_ENTRIES: u32 = 64;

/// Size of each read buffer submitted to `io_uring`.
pub const READ_BUF_SIZE: usize = 4096;

/// Size of the (single) in-flight write buffer.
const WRITE_BUF_SIZE: usize = 4096;

use inflight::{InFlight, OpTag};

/// `user_data` for the cancel SQEs submitted during teardown. Outside the
/// range [`OpTag`] decodes, so a cancel's own completion is never mistaken
/// for an operation completing.
const CANCEL_USER_DATA: u64 = 0xffff_0001;

/// How long teardown waits for the kernel to give the buffers back before
/// leaking them instead.
const QUIESCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// One message from the application into the PTY event loop.
///
/// The `next_input` closure handed to [`Pty::run`] translates whatever the
/// application's own ring carries into this shape.
pub enum PtyInput {
    /// Raw bytes to write to the PTY master (keystrokes, paste, replies).
    Bytes(Box<[u8]>),
    /// Push a new window size to the child (`TIOCSWINSZ` → SIGWINCH).
    Resize(WinSize),
    /// Stop the event loop and tear the child down.
    Quit,
}

/// Owned pseudoterminal handle.
///
/// Created by [`Pty::spawn`]. Drive I/O by calling [`Pty::run`] on a
/// dedicated thread.
pub struct Pty {
    /// `io_uring` queue driving all master-fd I/O.
    ///
    /// Declared before the I/O buffers below so the ring is torn down
    /// first on drop.
    ///
    /// Field order alone is **not** sufficient for soundness. Closing the
    /// ring fd starts the kernel's teardown but does not wait for it, so a
    /// read armed against `read_buf` can complete after that buffer is
    /// freed. [`Pty::quiesce`] cancels the outstanding operations and waits
    /// for their completions before anything is dropped.
    queue: UringQueue,
    /// SQ producer — for submitting read/write SQEs.
    sq: SqProducer,
    /// CQ consumer — drained in the event loop.
    cq: CqConsumer,
    /// Master fd of the pseudoterminal.
    master_fd: ququmatz::RawFd,
    /// PID of the child process.
    child_pid: ChildPid,
    /// Read buffer. Exactly one master-fd read SQE is in flight at a time,
    /// re-armed only after its CQE is consumed, so a single stable buffer
    /// suffices (heap-allocated: its address survives `Pty` being moved
    /// onto the event-loop thread).
    read_buf: Box<[u8; READ_BUF_SIZE]>,
    /// Write buffer for the (single) in-flight write SQE. Writes are
    /// serialized — one SQE at a time — because concurrent writes to the
    /// same fd carry no ordering guarantee and terminal input bytes must
    /// stay strictly ordered.
    write_buf: Box<[u8; WRITE_BUF_SIZE]>,
    /// Valid length of the in-flight write.
    write_len: usize,
    /// Whether a write SQE is currently in flight.
    write_in_flight: bool,
    /// Bytes queued behind the in-flight write.
    pending_write: VecDeque<u8>,
    /// `signalfd` fd for SIGCHLD.
    signal_fd: ququmatz::RawFd,
    /// Stable buffer the kernel writes `signalfd_siginfo` into.
    signal_buf: Box<[u8; 128]>,
    /// Wake eventfd (owned by the caller — NOT closed on teardown). A read
    /// SQE stays armed on it; producers signal it after queueing input.
    wake_fd: ququmatz::RawFd,
    /// Stable 8-byte buffer for the eventfd counter read. Reading the
    /// counter also resets it, so the read SQE doubles as the drain.
    wake_buf: Box<[u8; 8]>,
    /// Set by the caller (or another thread) to request a graceful shutdown.
    shutdown: Arc<AtomicBool>,
    /// Length of a completed read that `on_output` rejected (consumer ring
    /// full), still sitting in `read_buf` awaiting retry. While `Some`, the
    /// master-fd read SQE is deliberately left un-armed: not reading keeps
    /// the kernel's pty output buffer full, which back-pressures the child's
    /// `write(2)` for free instead of us silently dropping bytes.
    read_pending: Option<usize>,
    /// Which submitted operations the kernel has not completed yet, and so
    /// whose buffers it may still write into. Teardown must not free a
    /// buffer while its operation is in this set.
    in_flight: InFlight,
}

impl Pty {
    /// Spawn a shell under a new pseudoterminal.
    ///
    /// `wake_fd` is a caller-owned eventfd: signal it after queueing input
    /// so the event loop wakes and drains `next_input`. It is polled via an
    /// armed read SQE and is **not** closed by this type.
    ///
    /// `shutdown` is checked once per loop iteration; set it *and* signal
    /// `wake_fd` to stop the loop from another thread.
    ///
    /// SIGCHLD is blocked on the calling thread and routed to a `signalfd`.
    /// For reliable delivery the signal should be blocked on **every**
    /// thread — call [`RawPty::block_sigchld`] on the main thread before
    /// spawning any others, so the mask is inherited.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if any syscall fails.
    pub fn spawn(
        shell: &str,
        args: &[&str],
        win_size: WinSize,
        wake_fd: i32,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self, PtyError> {
        let (master_fd, child_pid) = sys::RawPty::fork(shell, args, win_size)?;
        let signal_fd = sys::RawPty::signalfd_for_sigchld()?;

        let (queue, sq, cq) = UringQueue::new(RING_ENTRIES).map_err(PtyError::UringSetup)?;

        Ok(Self {
            queue,
            sq,
            cq,
            master_fd,
            child_pid,
            read_buf: Box::new([0u8; READ_BUF_SIZE]),
            write_buf: Box::new([0u8; WRITE_BUF_SIZE]),
            write_len: 0,
            write_in_flight: false,
            pending_write: VecDeque::new(),
            signal_fd,
            signal_buf: Box::new([0u8; 128]),
            // A valid fd is always non-negative (checked by the OS at
            // open/eventfd time), so this cast can't lose the sign.
            wake_fd: ququmatz::RawFd::from_raw(wake_fd.cast_unsigned() as usize),
            wake_buf: Box::new([0u8; 8]),
            shutdown,
            read_pending: None,
            in_flight: InFlight::new(),
        })
    }

    /// PID of the forked shell child.
    ///
    /// Lets a caller resolve the shell's live working directory via
    /// `/proc/<pid>/cwd` (or that of its foreground child, for "open a new
    /// terminal in the directory a command is currently running in") --
    /// e.g. to implement Ctrl+Shift+Enter/"open here" without any external
    /// IPC or window-manager cooperation.
    #[must_use]
    pub const fn child_pid(&self) -> i32 {
        self.child_pid.0
    }

    /// Resize the terminal window.
    ///
    /// Safe to call from any thread at any time (it's a plain ioctl); the
    /// event loop also applies [`PtyInput::Resize`] messages itself.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if the `TIOCSWINSZ` ioctl fails.
    pub fn resize(&self, win_size: WinSize) -> Result<(), PtyError> {
        self.master_fd.set_win_size(win_size)
    }

    /// Run the PTY I/O event loop. **Blocks the calling thread until the
    /// child exits, `next_input` yields [`PtyInput::Quit`], or the shutdown
    /// flag is set (plus a wake signal).**
    ///
    /// `next_input` is polled to exhaustion after every wakeup; resizes are
    /// coalesced within a drain (only the last is applied) so an
    /// interactive resize drag doesn't storm the shell with `SIGWINCH`es.
    ///
    /// `on_output` receives each chunk of PTY output, at most
    /// [`READ_BUF_SIZE`] bytes per call, in order. It returns `true` if the
    /// chunk was accepted, `false` if the caller's downstream ring is full.
    /// On rejection the master-fd read SQE is **not** re-armed: the loop
    /// retries the very same chunk (via [`Pty::retry_output`]-style logic on
    /// the next wakeup, e.g. the wake eventfd firing once the consumer has
    /// drained) instead of dropping bytes. Leaving the read un-armed lets the
    /// kernel's pty output buffer fill up and naturally throttle the child's
    /// `write(2)` calls — real backpressure instead of silent data loss.
    ///
    /// Returns the child's exit code on success (0 when the loop was
    /// stopped before the child exited).
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] on unrecoverable `io_uring` errors.
    pub fn run<I, O>(mut self, mut next_input: I, mut on_output: O) -> Result<i32, PtyError>
    where
        I: FnMut() -> Option<PtyInput>,
        O: FnMut(&[u8]) -> bool,
    {
        self.arm_read()?;
        self.arm_signal()?;
        self.arm_wake()?;

        let mut exit_code: i32 = 0;
        // Whether the child has already been reaped via SIGCHLD (skip the
        // hangup+reap teardown path in that case).
        let mut reaped = false;
        // Whether the loop stopped because the master fd hit EOF/EIO (the
        // child exited of its own accord, vs. a caller-requested shutdown).
        let mut got_eof = false;

        // Drain any input queued before the loop started (the producer may
        // have signalled the eventfd before our wake SQE was armed).
        let mut quit_requested = self.drain_input(&mut next_input);
        self.flush_write()?;

        'event_loop: loop {
            if quit_requested || self.shutdown.load(Ordering::Acquire) {
                break 'event_loop;
            }

            // A previous chunk was rejected by the consumer ring — retry it
            // before parking. Cheap, and closes the race where the wake
            // signal that unblocks us below arrived because the consumer
            // just freed the space this very chunk needs.
            if let Some(len) = self.read_pending
                && on_output(&self.read_buf[..len])
            {
                self.read_pending = None;
                self.arm_read()?;
            }

            // Park in the kernel until at least one CQE is ready. Note: if
            // `read_pending` is still `Some`, no read SQE is armed, so this
            // can only wake on the signalfd or the wake eventfd — exactly
            // the throttle we want (the kernel's pty buffer backpressures
            // the child meanwhile). The producer side must signal the wake
            // eventfd once it frees ring space so this doesn't stall.
            self.queue
                .submit_and_wait(1)
                .map_err(PtyError::UringEnter)?;

            // Collect completions first — we can't borrow self mutably
            // inside the drain closure. Unbounded Vec: a burst of
            // completions must never be truncated and lose a re-arm.
            let mut actions: Vec<CqeAction> = Vec::new();
            let mut completed: Vec<OpTag> = Vec::new();
            let cq = &self.cq;
            cq.drain(|cqe| {
                if let Some(tag) = OpTag::from_user_data(cqe.user_data) {
                    completed.push(tag);
                }
                actions.push(CqeAction::from_cqe(cqe.user_data, cqe.result));
            });
            // Clear before dispatch: an action may re-arm the very tag whose
            // completion this batch carried.
            for tag in completed {
                self.in_flight.complete(tag);
            }

            let mut got_exit = false;
            for action in &actions {
                match *action {
                    CqeAction::ReadOk(len) => {
                        // If on_output rejects (consumer ring full), leave
                        // the read un-armed and remember the chunk for a
                        // retry — the kernel's pty buffer then throttles the
                        // child's writes instead of us dropping data.
                        if on_output(&self.read_buf[..len]) {
                            self.arm_read()?;
                        } else {
                            self.read_pending = Some(len);
                        }
                    }
                    CqeAction::ReadRetry => {
                        // Transient errno (EINTR/EAGAIN) — nothing was read;
                        // re-arm and keep going. Treating this as EOF (the
                        // old behaviour) tore the whole session down on a
                        // stray signal.
                        self.arm_read()?;
                    }
                    CqeAction::ReadEof => {
                        // EOF or EIO on the master fd: the child closed the
                        // slave side (it exited or is exiting). The SIGCHLD
                        // completion — possibly in this same batch, possibly
                        // not yet delivered — carries the actual exit code;
                        // the post-loop reap below covers the race where the
                        // EOF CQE wins.
                        got_eof = true;
                        got_exit = true;
                    }
                    CqeAction::WriteDone(result) => {
                        self.complete_write(result);
                    }
                    CqeAction::Signal => match self.child_pid.wait_nohang() {
                        Ok(sys::WaitStatus::Exited(code)) => {
                            exit_code = code;
                            reaped = true;
                            got_exit = true;
                        }
                        Ok(sys::WaitStatus::Signaled(sig)) => {
                            // Killed by a signal: report the shell
                            // convention (128 + signum).
                            exit_code = 128 + sig;
                            reaped = true;
                            got_exit = true;
                        }
                        Ok(sys::WaitStatus::StillRunning) => {
                            // Spurious SIGCHLD (stop/continue) — re-arm.
                            self.arm_signal()?;
                        }
                        Err(_) => {
                            // wait4 failed: treat as exited so we don't spin
                            // re-arming a signalfd that can't tell us
                            // anything, but don't fabricate a success code.
                            exit_code = -1;
                            got_exit = true;
                        }
                    },
                    CqeAction::Wake => {
                        // Re-arm FIRST, then drain below: any signal that
                        // lands after the drain finds the counter watched by
                        // an armed SQE, so no wakeup is ever lost.
                        self.arm_wake()?;
                    }
                    CqeAction::None => {}
                }
            }

            if got_exit {
                break 'event_loop;
            }

            // Drain queued input once per wakeup (not just on Wake CQEs:
            // it's cheap and closes any drain-vs-signal race) and kick the
            // write path.
            quit_requested = self.drain_input(&mut next_input);
            self.flush_write()?;
        }

        // The master fd can report EOF before the SIGCHLD completion is
        // seen — the child is exiting either way, so reap it synchronously
        // and recover the real exit code instead of returning a bogus 0.
        if got_eof && !reaped {
            match self.child_pid.reap_blocking_status() {
                Some(sys::WaitStatus::Exited(code)) => exit_code = code,
                Some(sys::WaitStatus::Signaled(sig)) => exit_code = 128 + sig,
                // StillRunning can't come from a blocking wait4; None means
                // wait4 failed (e.g. already reaped by a racing SIGCHLD
                // handler) — keep whatever code we have.
                Some(sys::WaitStatus::StillRunning) | None => {}
            }
            reaped = true;
        }

        // Before anything is freed: take the buffers back from the kernel.
        // This runs while the fds are still open, since cancelling an
        // operation needs the ring fd it was submitted to.
        let quiet = self.quiesce();

        self.cleanup(reaped);

        if !quiet {
            // The kernel never returned some completion, so it may still
            // write into these buffers. Leak them rather than hand the
            // allocator memory the kernel is using.
            std::mem::forget(self);
        }
        Ok(exit_code)
    }

    // ── private helpers ────────────────────────────────────────────────────

    /// Pop everything `next_input` has, buffering bytes and coalescing
    /// resizes (only the last one is applied — an interactive drag fires a
    /// storm of configures but the shell only needs one SIGWINCH at the
    /// final size). Returns `true` if a [`PtyInput::Quit`] was seen.
    fn drain_input<I: FnMut() -> Option<PtyInput>>(&mut self, next_input: &mut I) -> bool {
        let mut pending_resize: Option<WinSize> = None;
        let mut quit = false;
        while let Some(msg) = next_input() {
            match msg {
                PtyInput::Bytes(b) => self.pending_write.extend(b),
                PtyInput::Resize(ws) => pending_resize = Some(ws),
                PtyInput::Quit => quit = true,
            }
        }
        if let Some(ws) = pending_resize {
            let _ = self.master_fd.set_win_size(ws);
        }
        quit
    }

    /// Submit the next write SQE if none is in flight and bytes are pending.
    fn flush_write(&mut self) -> Result<(), PtyError> {
        if self.write_in_flight || self.pending_write.is_empty() {
            return Ok(());
        }
        // Batch-copy from the deque's two contiguous halves instead of a
        // per-byte pop_front (which pays a wraparound branch every byte —
        // noticeable when a large paste floods pending_write).
        let len = WRITE_BUF_SIZE.min(self.pending_write.len());
        let (head, tail) = self.pending_write.as_slices();
        let from_head = len.min(head.len());
        self.write_buf[..from_head].copy_from_slice(&head[..from_head]);
        self.write_buf[from_head..len].copy_from_slice(&tail[..len - from_head]);
        self.pending_write.drain(..len);
        self.write_len = len;
        self.write_in_flight = true;
        // `len` is bounded by `WRITE_BUF_SIZE`, a small compile-time buffer
        // size, so this always fits in u32.
        let len_u32 = u32::try_from(len).unwrap_or(u32::MAX);
        // SAFETY: write_buf is a stable heap allocation owned by self; it is
        // only rewritten after the WriteDone CQE for this SQE is consumed
        // (single-write-in-flight invariant via write_in_flight).
        let sqe =
            unsafe { Sqe::write_ptr(self.master_fd, self.write_buf.as_ptr(), len_u32, u64::MAX) }
                .user_data(OpTag::Write.as_user_data());
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.in_flight.arm(OpTag::Write);
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    /// Handle a write CQE: requeue the unwritten tail of a short write (in
    /// front of anything still pending, preserving byte order).
    ///
    /// A negative result is a raw `-errno`. Transient errnos — `EAGAIN`
    /// (the PTY line discipline's input queue is momentarily full, which a
    /// large burst write routinely hits) and `EINTR` — mean *zero* bytes of
    /// this SQE were actually written, so the whole buffer must be requeued
    /// or those bytes vanish silently (this was a real bug: a large write
    /// into a slow reader could drop a whole in-flight chunk and never
    /// surface an error). Only a fatal errno (`EIO`/`EBADF`/etc — the
    /// child/slave side is actually gone) drops the buffer, since requeueing
    /// that would spin forever.
    fn complete_write(&mut self, result: i32) {
        const EAGAIN: i32 = 11;
        const EINTR: i32 = 4;
        if result >= 0 {
            // Checked non-negative above, so the sign-preserving cast is exact.
            let written = result.cast_unsigned() as usize;
            if written < self.write_len {
                for i in (written..self.write_len).rev() {
                    self.pending_write.push_front(self.write_buf[i]);
                }
            }
        } else if result == -EAGAIN || result == -EINTR {
            // Nothing was written — requeue the entire buffer in order.
            for i in (0..self.write_len).rev() {
                self.pending_write.push_front(self.write_buf[i]);
            }
        }
        self.write_in_flight = false;
    }

    fn arm_read(&mut self) -> Result<(), PtyError> {
        // READ_BUF_SIZE is a small compile-time buffer size, always fits in u32.
        let read_buf_size_u32 = u32::try_from(READ_BUF_SIZE).expect("READ_BUF_SIZE fits in u32");
        // SAFETY: read_buf is a stable heap allocation owned by self, valid
        // until the next arm_read — which only happens after this SQE's CQE
        // is consumed (single-read-in-flight invariant).
        let sqe = unsafe {
            Sqe::read_ptr(
                self.master_fd,
                self.read_buf.as_ptr().cast_mut(),
                read_buf_size_u32,
                u64::MAX,
            )
        }
        .user_data(OpTag::Read.as_user_data());
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.in_flight.arm(OpTag::Read);
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    fn arm_signal(&mut self) -> Result<(), PtyError> {
        // SAFETY: signal_buf's heap allocation outlives this call (owned by
        // Pty, which outlives the loop in `run`); only one signal read is
        // ever in flight against it.
        let sqe = unsafe {
            Sqe::read_ptr(
                self.signal_fd,
                self.signal_buf.as_ptr().cast_mut(),
                128,
                u64::MAX,
            )
        }
        .user_data(OpTag::Signal.as_user_data());
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.in_flight.arm(OpTag::Signal);
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    fn arm_wake(&mut self) -> Result<(), PtyError> {
        // Reading an eventfd returns the counter AND resets it to zero, so
        // this armed read both wakes the loop and drains the count — several
        // coalesced signals cost one CQE.
        // SAFETY: wake_buf is a stable heap allocation owned by self; only
        // one wake read is ever in flight (re-armed after its CQE).
        let sqe =
            unsafe { Sqe::read_ptr(self.wake_fd, self.wake_buf.as_ptr().cast_mut(), 8, u64::MAX) }
                .user_data(OpTag::Wake.as_user_data());
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.in_flight.arm(OpTag::Wake);
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    /// Cancel every outstanding operation and wait for its completion, so
    /// the kernel owns none of our buffers when they are freed.
    ///
    /// Closing the ring fd is **not** enough on its own. The kernel's ring
    /// teardown is asynchronous, so a read armed against `read_buf` can
    /// still complete after `Drop` has returned that allocation to the
    /// heap. The kernel then writes into a free chunk, and the corruption
    /// surfaces much later as an allocator abort in an unrelated thread
    /// (`malloc(): unaligned tcache chunk`). The completion of an operation
    /// is the only proof that the kernel has let its buffer go.
    ///
    /// If the kernel does not return every completion within
    /// [`QUIESCE_TIMEOUT`], this reports failure and the caller leaks the
    /// buffers instead. A bounded leak on a teardown path is recoverable;
    /// freeing memory the kernel still writes into is not.
    fn quiesce(&mut self) -> bool {
        if self.in_flight.is_quiet() {
            return true;
        }

        for tag in self.in_flight.armed() {
            let sqe = Sqe::cancel(tag.as_user_data()).user_data(CANCEL_USER_DATA);
            if self.sq.push(sqe).is_err() {
                break;
            }
        }
        let _ = self.queue.submit();

        // Poll rather than block in the kernel. `submit_and_wait(1)` sleeps
        // until a completion arrives, which makes the deadline below
        // unenforceable: an operation the kernel declines to cancel would
        // hang teardown forever instead of falling back to the leak. A
        // cancelled operation completes almost immediately, so this loop
        // normally runs a single pass.
        let deadline = std::time::Instant::now() + QUIESCE_TIMEOUT;
        while !self.in_flight.is_quiet() {
            let _ = self.queue.submit();
            let in_flight = &mut self.in_flight;
            // A cancelled operation still posts a CQE (with `-ECANCELED`),
            // so its arrival is what confirms the buffer is idle.
            self.cq.drain(|cqe| {
                if let Some(tag) = OpTag::from_user_data(cqe.user_data) {
                    in_flight.complete(tag);
                }
            });
            if self.in_flight.is_quiet() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        true
    }

    fn cleanup(&self, reaped: bool) {
        self.queue.close();
        if !reaped {
            // Shutdown requested before the child exited: hang up the shell
            // and reap it so it isn't orphaned holding the PTY slave open.
            self.child_pid.hangup();
            self.child_pid.reap_blocking();
        }
        let _ = self.master_fd.close();
        let _ = self.signal_fd.close();
        // wake_fd is caller-owned (their eventfd/Waker) — never closed here.
    }
}

// ── CQE action dispatch ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum CqeAction {
    None,
    ReadOk(usize),
    /// Master-fd read completed with a transient errno (`EINTR`/`EAGAIN`) —
    /// no data, no EOF; just re-arm the read.
    ReadRetry,
    /// Master-fd read returned 0 (EOF) or an error (typically EIO once the
    /// child exits and the slave side closes) — both mean "child gone".
    ReadEof,
    /// Write CQE with its raw result (bytes written, or negative errno).
    WriteDone(i32),
    Signal,
    Wake,
}

impl CqeAction {
    const fn from_cqe(user_data: u64, result: i32) -> Self {
        match OpTag::from_user_data(user_data) {
            Some(OpTag::Read) => {
                const EAGAIN: i32 = 11;
                const EINTR: i32 = 4;
                if result > 0 {
                    // Checked positive above, so the sign-preserving cast is exact.
                    Self::ReadOk(result.cast_unsigned() as usize)
                } else if result == -EAGAIN || result == -EINTR {
                    Self::ReadRetry
                } else {
                    // 0 (EOF) or a fatal errno (typically -EIO once the
                    // child exits and the slave side closes) — child gone.
                    Self::ReadEof
                }
            }
            Some(OpTag::Write) => Self::WriteDone(result),
            Some(OpTag::Signal) => Self::Signal,
            Some(OpTag::Wake) => Self::Wake,
            None => Self::None,
        }
    }
}
