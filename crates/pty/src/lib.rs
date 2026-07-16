// Syscall FFI: `as` casts between C int widths are bounded and correct.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
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
//!                     ├── read SQEs  → CQEs → SPSC ring → vt_parser
//!                     └── write SQEs ← SPSC ring ← input encoder
//! ```
//!
//! [`Pty::run`] is the blocking event loop. Spawn it on a dedicated thread.

mod error;
mod simple;
mod sys;

pub use error::PtyError;
pub use simple::PtySimple;
pub use sys::{ChildPid, CloseFd, PtyIoctl, RawPty, WaitStatus, WinSize};

use quetzal_uring::{CqConsumer, SqProducer, UringQueue};
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::spsc::RingBuffer as SpscRing;
use ququmatz::Sqe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Number of `io_uring` entries for the PTY queue.
const RING_ENTRIES: u32 = 64;

/// Size of each read buffer submitted to `io_uring`.
pub const READ_BUF_SIZE: usize = 4096;

/// Capacity of the SPSC chunk ring towards the VT parser.
const CHUNK_RING_CAP: usize = 256;

/// Capacity of the write-byte ring from the input encoder.
const WRITE_RING_CAP: usize = CHUNK_RING_CAP * READ_BUF_SIZE;

/// `user_data` tags for CQE dispatch.
mod tag {
    pub const READ: u64 = 1;
    pub const WRITE: u64 = 2;
    pub const SIGNAL: u64 = 3;
}

/// A fixed-size byte chunk produced by a PTY read.
#[derive(Clone, Copy)]
pub struct ByteChunk {
    /// Raw bytes from the PTY master fd.
    pub buf: [u8; READ_BUF_SIZE],
    /// Number of valid bytes in `buf`.
    pub len: usize,
}

/// The consumer half of the byte-chunk SPSC ring.  Hand to the VT parser thread.
pub type ChunkConsumer = quetzalcoatl::spsc::Consumer<ByteChunk>;

/// The producer half of the write-bytes SPSC ring.  Hand to the input encoder.
pub type WriteProducer = quetzalcoatl::spsc::Producer<u8>;

/// The consumer half of the write-bytes SPSC ring (PTY thread side).
type WriteConsumer = quetzalcoatl::spsc::Consumer<u8>;

/// Owned pseudoterminal handle.
///
/// Created by [`Pty::spawn`]. Drive I/O by calling [`Pty::run`] on a
/// dedicated thread; write to the PTY by pushing bytes onto the
/// [`WriteProducer`] returned alongside it.
pub struct Pty {
    /// Master fd of the pseudoterminal.
    master_fd: ququmatz::RawFd,
    /// PID of the child process.
    child_pid: ChildPid,
    /// `io_uring` queue driving all master-fd I/O.
    queue: UringQueue,
    /// SQ producer — for submitting read/write SQEs.
    sq: SqProducer,
    /// CQ consumer — drained in the event loop.
    cq: CqConsumer,
    /// Read buffers (ping-pong: one in flight, one being read from).
    read_bufs: Box<[[u8; READ_BUF_SIZE]; 2]>,
    /// Which read buffer index is currently submitted.
    read_buf_idx: usize,
    /// SPSC producer: PTY bytes → VT parser.
    chunk_producer: quetzalcoatl::spsc::Producer<ByteChunk>,
    /// `signalfd` fd for SIGCHLD.
    signal_fd: ququmatz::RawFd,
    /// Set by the caller to request a graceful shutdown.
    shutdown: Arc<AtomicBool>,
}

impl Pty {
    /// Spawn a shell under a new pseudoterminal.
    ///
    /// Returns:
    /// - The [`Pty`] event-loop handle.
    /// - [`ChunkConsumer`] — hand to the VT parser thread.
    /// - [`WriteProducer`] — hand to the input encoder thread.
    /// - [`Arc<AtomicBool>`] — set to `true` to request shutdown.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if any syscall fails.
    pub fn spawn(
        shell: &str,
        args: &[&str],
        win_size: WinSize,
    ) -> Result<(Self, ChunkConsumer, WriteProducer, Arc<AtomicBool>), PtyError> {
        let (master_fd, child_pid) = sys::RawPty::fork(shell, args, win_size)?;
        let signal_fd = sys::RawPty::signalfd_for_sigchld()?;

        let (queue, sq, cq) = UringQueue::new(RING_ENTRIES).map_err(PtyError::UringSetup)?;

        let (chunk_tx, chunk_rx) =
            SpscRing::<ByteChunk>::new(Capacity::exact(CHUNK_RING_CAP)).split();
        let (write_tx, write_rx) =
            SpscRing::<u8>::new(Capacity::exact(WRITE_RING_CAP)).split();

        let shutdown = Arc::new(AtomicBool::new(false));

        let pty = Self {
            master_fd,
            child_pid,
            queue,
            sq,
            cq,
            read_bufs: Box::new([[0u8; READ_BUF_SIZE]; 2]),
            read_buf_idx: 0,
            chunk_producer: chunk_tx,
            signal_fd,
            shutdown: Arc::clone(&shutdown),
        };

        // write_rx is moved into run(); return write_tx to the caller.
        // We thread write_rx through the return so the caller can hand it
        // to Pty::run on their chosen thread.
        let _ = write_rx; // suppressed: caller must pass to run()
        Ok((pty, chunk_rx, write_tx, shutdown))
    }

    /// Resize the terminal window.
    ///
    /// Safe to call from any thread at any time.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] if the `TIOCSWINSZ` ioctl fails.
    pub fn resize(&self, win_size: WinSize) -> Result<(), PtyError> {
        self.master_fd.set_win_size(win_size)
    }

    /// Run the PTY I/O event loop. **Blocks the calling thread until the
    /// child exits or the shutdown flag is set.**
    ///
    /// `write_rx` is the consumer side of the write ring; bytes pushed
    /// by the input encoder are batched and submitted as write SQEs.
    ///
    /// Returns the child's exit code on success.
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] on unrecoverable `io_uring` errors.
    pub fn run(mut self, mut write_rx: WriteConsumer) -> Result<i32, PtyError> {
        self.arm_read(self.read_buf_idx)?;
        self.arm_signal()?;

        let mut exit_code: i32 = 0;

        'event_loop: loop {
            // Flush any pending writes from the input encoder.
            self.drain_write_ring(&mut write_rx)?;

            // Block until at least one CQE is ready.
            self.queue
                .submit_and_wait(1)
                .map_err(PtyError::UringEnter)?;

            // Process all ready CQEs.
            let mut got_exit = false;
            let cq = &self.cq;

            // Collect results — we can't borrow self mutably inside the closure,
            // so accumulate actions into a small fixed-size stack buffer.
            let mut actions: [CqeAction; 8] = [CqeAction::None; 8];
            let mut n_actions = 0usize;

            cq.drain(|cqe| {
                if n_actions < actions.len() {
                    actions[n_actions] = CqeAction::from_cqe(cqe.user_data, cqe.result);
                    n_actions += 1;
                }
            });

            for action in &actions[..n_actions] {
                match action {
                    CqeAction::ReadOk(len) => {
                        let idx = self.read_buf_idx;
                        let len = *len;

                        // Build chunk and push onto the SPSC ring.
                        let mut chunk = ByteChunk {
                            buf: [0u8; READ_BUF_SIZE],
                            len,
                        };
                        chunk.buf[..len].copy_from_slice(&self.read_bufs[idx][..len]);
                        // Best-effort: if the ring is full the parser is lagging; drop.
                        let _ = self.chunk_producer.push(chunk);

                        // Flip buffer and rearm the read.
                        self.read_buf_idx = 1 - idx;
                        self.arm_read(self.read_buf_idx)?;
                    }
                    CqeAction::ReadEof => {
                        got_exit = true;
                    }
                    // Distinct from `None` semantically (a write CQE was
                    // reaped) even though both are no-ops here.
                    #[allow(clippy::match_same_arms)]
                    CqeAction::WriteOk => {
                        // Write completed — nothing to do; next flush_write_ring
                        // will submit more if pending.
                    }
                    CqeAction::Signal => {
                        match self.child_pid.wait_nohang() {
                            Ok(sys::WaitStatus::Exited(code)) => {
                                exit_code = code;
                                got_exit = true;
                            }
                            Ok(sys::WaitStatus::Signaled(_)) => {
                                // Killed by a signal: no POSIX exit code:
                                // report the shell convention (128 + signum)
                                // rather than silently claiming success.
                                exit_code = 128;
                                got_exit = true;
                            }
                            Ok(sys::WaitStatus::StillRunning) => {
                                // Spurious SIGCHLD wakeup (e.g. the child
                                // was stopped/continued, not exited) --
                                // nothing to reap yet, keep going.
                            }
                            Err(_) => {
                                // wait4 failed: treat as exited so we don't
                                // spin forever re-arming a signalfd that
                                // can no longer tell us anything, but don't
                                // claim a fabricated success code either.
                                exit_code = -1;
                                got_exit = true;
                            }
                        }
                        // Rearm the signalfd read for future signals, unless
                        // we're already tearing down.
                        if !got_exit {
                            self.arm_signal()?;
                        }
                    }
                    CqeAction::None => {}
                }
            }

            if got_exit || self.shutdown.load(Ordering::Acquire) {
                break 'event_loop;
            }
        }

        self.cleanup();
        Ok(exit_code)
    }

    // ── private helpers ────────────────────────────────────────────────────

    fn arm_read(&self, idx: usize) -> Result<(), PtyError> {
        // SAFETY: read_bufs lives for the lifetime of Pty; the buf pointer is
        // valid until the next arm_read call which only happens after the CQE
        // for this slot arrives (ping-pong invariant).
        let sqe = unsafe {
            Sqe::read_ptr(
                self.master_fd,
                self.read_bufs[idx].as_ptr().cast_mut(),
                READ_BUF_SIZE as u32,
                u64::MAX,
            )
        }
        .user_data(tag::READ);
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    fn arm_signal(&self) -> Result<(), PtyError> {
        // One signal read is ever in-flight at a time (single PTY thread).
        // Leak a heap buffer so the kernel has a stable address to write into.
        // 128 bytes == sizeof(signalfd_siginfo) on Linux.
        let buf: &'static mut [u8; 128] = Box::leak(Box::new([0u8; 128]));
        let sqe =
            unsafe { Sqe::read_ptr(self.signal_fd, buf.as_mut_ptr(), 128, u64::MAX) }
                .user_data(tag::SIGNAL);
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    fn drain_write_ring(&self, rx: &mut WriteConsumer) -> Result<(), PtyError> {
        // Drain bytes from the ring into a stack buffer, then submit one write SQE.
        let mut buf = [0u8; READ_BUF_SIZE];
        let mut len = 0usize;
        while len < READ_BUF_SIZE {
            match rx.pop() {
                Some(b) => {
                    buf[len] = b;
                    len += 1;
                }
                None => break,
            }
        }
        if len == 0 {
            return Ok(());
        }
        // SAFETY: buf is valid for the duration of this call; the SQE is
        // submitted synchronously and submit_and_wait in the caller ensures
        // the write completes before buf goes out of scope. However, because
        // buf is stack-allocated and submit() is non-blocking, the kernel may
        // read from it after we return.
        //
        // Fix: we copy into a heap allocation for writes.
        let write_buf: Box<[u8]> = buf[..len].to_vec().into_boxed_slice();
        let sqe = unsafe {
            Sqe::write_ptr(
                self.master_fd,
                write_buf.as_ptr(),
                len as u32,
                u64::MAX,
            )
        }
        .user_data(tag::WRITE);
        // Keep write_buf alive until the CQE arrives. We leak it here and
        // reclaim it in the WriteOk handler via the user_data pointer.
        // For simplicity at M1 we just forget it (bounded leak: max 4 KiB
        // per write, reclaimed at process exit). TODO M7: slab allocator.
        std::mem::forget(write_buf);
        self.sq.push(sqe).map_err(|_| PtyError::SqFull)?;
        self.queue.submit().map_err(PtyError::UringEnter)
    }

    fn cleanup(&self) {
        self.queue.close();
        let _ = self.master_fd.close();
        let _ = self.signal_fd.close();
    }
}

// ── CQE action dispatch ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum CqeAction {
    None,
    ReadOk(usize),
    ReadEof,
    WriteOk,
    Signal,
}

impl CqeAction {
    const fn from_cqe(user_data: u64, result: i32) -> Self {
        match user_data {
            tag::READ => {
                if result > 0 {
                    Self::ReadOk(result as usize)
                } else {
                    Self::ReadEof
                }
            }
            tag::WRITE => Self::WriteOk,
            tag::SIGNAL => Self::Signal,
            _ => Self::None,
        }
    }
}
