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
//!    └── parent → Pty::run creates the io_uring on its own thread
//!                     │
//!                     ├── multishot read (master fd) → CQE per chunk → `on_output`
//!                     ├── write SQE  (master fd)  ← bytes from `next_input`
//!                     ├── poll SQE   (master fd)  → retry a write that hit EAGAIN
//!                     ├── read SQE   (signalfd)   → SIGCHLD → child reaped
//!                     └── read SQE   (wake eventfd) → drain `next_input`
//! ```
//!
//! [`Pty::run`] is the blocking event loop: it parks in `io_uring_enter`
//! until *any* of the four sources completes — no `poll(2)`, no
//! busy-spinning, no auxiliary wait primitive. Spawn it on a dedicated
//! thread.
//!
//! The ring is created inside `run`, so the thread that runs the loop is
//! the one that created it. That lets the ring use `DEFER_TASKRUN`: the
//! kernel queues completion work on the ring and runs it only when the loop
//! enters the kernel to wait. Without it the kernel signals the thread to
//! run that work, and a tty write that sees the signal pending stops with
//! `-EINTR`, costing a resubmit.
//!
//! The caller owns the message transport: `run` pulls input via a
//! `next_input` closure and hands PTY output to an `on_output` closure, so
//! this crate stays agnostic of whichever ring buffers the application
//! wires between its threads. Producers must signal the wake eventfd
//! (passed to [`Pty::spawn`]) after queueing input; the armed eventfd read
//! SQE completes and the loop drains.
//!
//! The master fd is non-blocking. A tty read or write on a *blocking* fd
//! runs inline inside `io_uring_enter` and sleeps there when the tty buffer
//! is full or empty, ignoring `io_uring`'s no-wait request. A write blocked
//! that way stops the loop from reading the child's output, the child then
//! blocks writing that output and stops reading its input, and both sides
//! wait forever. With `O_NONBLOCK` such a write returns `-EAGAIN` instead;
//! the loop then arms a `POLL_ADD` and retries once it fires.
//!
//! The master-fd read is one multishot SQE (kernel 6.7+) that fills buffers
//! from a provided-buffer pool and posts a CQE per chunk, so output costs
//! no re-arm per read. The kernel waits for data itself.
//!
//! [`PtySimple`] (plain non-blocking syscalls) is retained for tests and as
//! a reference implementation.

mod action;
mod error;
mod inflight;
mod outcome;
mod session;
mod simple;
mod spawned;
mod stats;
mod sys;
mod write_queue;

pub use error::PtyError;
pub use simple::PtySimple;
pub use stats::{ErrnoCounts, Stats, StatsSnapshot};
pub use sys::{ChildPid, CloseFd, PtyIoctl, RawPty, WaitStatus, WinSize};

use session::Session;
use spawned::Spawned;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use sys::NonBlocking;

/// Size of each buffer in the read pool.
pub const READ_BUF_SIZE: usize = 4096;

pub(crate) const EINTR: i32 = 4;
pub(crate) const EAGAIN: i32 = 11;
pub(crate) const ENOBUFS: i32 = 105;

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
    spawned: Spawned,
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
        master_fd.set_nonblock()?;
        let signal_fd = sys::RawPty::signalfd_for_sigchld()?;

        let spawned = Spawned {
            master_fd,
            child_pid,
            signal_fd,
            // A valid fd is always non-negative (checked by the OS at
            // open/eventfd time), so this cast can't lose the sign.
            wake_fd: ququmatz::RawFd::from_raw(wake_fd.cast_unsigned() as usize),
            shutdown,
        };
        Ok(Self { spawned })
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
        self.spawned.child_pid.0
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
        self.spawned.resize(win_size)
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
    /// retries the very same chunk on the next wakeup (e.g. the wake
    /// eventfd firing once the consumer has drained) instead of dropping
    /// bytes. Leaving the read un-armed lets the kernel's pty output buffer
    /// fill up and naturally throttle the child's `write(2)` calls — real
    /// backpressure instead of silent data loss.
    ///
    /// Returns the child's exit code on success (0 when the loop was
    /// stopped before the child exited).
    ///
    /// # Errors
    ///
    /// Returns [`PtyError`] on unrecoverable `io_uring` errors, including
    /// failing to create the ring. The child is hung up and reaped then.
    pub fn run<I, O>(self, next_input: I, on_output: O) -> Result<i32, PtyError>
    where
        I: FnMut() -> Option<PtyInput>,
        O: FnMut(&[u8]) -> bool,
    {
        Session::new(self.spawned)?.run(next_input, on_output)
    }
}
