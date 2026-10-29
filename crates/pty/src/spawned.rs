//! The forked child and the fds the event loop runs on.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::sys::{ChildPid, PtyIoctl};
use crate::{CloseFd, PtyError, WinSize};

pub struct Spawned {
    /// Master fd of the pseudoterminal.
    pub master_fd: ququmatz::RawFd,
    /// PID of the child process.
    pub child_pid: ChildPid,
    /// `signalfd` fd for SIGCHLD.
    pub signal_fd: ququmatz::RawFd,
    /// Wake eventfd (owned by the caller — NOT closed on teardown). A read
    /// SQE stays armed on it; producers signal it after queueing input.
    pub wake_fd: ququmatz::RawFd,
    /// Set by the caller (or another thread) to request a graceful shutdown.
    pub shutdown: Arc<AtomicBool>,
}

impl Spawned {
    pub fn resize(&self, win_size: WinSize) -> Result<(), PtyError> {
        self.master_fd.set_win_size(win_size)
    }

    /// Close the fds this type owns, first hanging up and reaping the
    /// child unless it was already reaped.
    pub fn teardown(&self, reaped: bool) {
        if !reaped {
            // Hang up the shell and reap it so it isn't orphaned holding
            // the PTY slave open.
            self.child_pid.hangup();
            self.child_pid.reap_blocking();
        }
        let _ = self.master_fd.close();
        let _ = self.signal_fd.close();
        // wake_fd is caller-owned (their eventfd/Waker) — never closed here.
    }
}
