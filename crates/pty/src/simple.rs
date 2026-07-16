#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
//! A dead-simple synchronous PTY wrapper for the M4 integration prototype.
//!
//! Uses raw `read(2)` / `write(2)` syscalls (non-blocking) instead of
//! `io_uring`, so it can be driven from a single event-loop thread without
//! spawning a PTY thread. Replaced by `Pty` + ring buffers in M5.

use crate::error::PtyError;
use crate::sys::{self, ChildPid, CloseFd, PtyIoctl, Syscall, WinSize};
use ququmatz::RawFd;

/// A simple blocking-ish PTY handle for the integration prototype.
pub struct PtySimple {
    master_fd: RawFd,
    child_pid: ChildPid,
}

impl Drop for PtySimple {
    /// Tear down cleanly: hang up the child shell, reap it, then close the
    /// master fd. Without this the shell is orphaned (reparented to init) and
    /// keeps the PTY slave — and on Wayland the leftover client connection —
    /// alive, leaving a ghost window after we exit.
    fn drop(&mut self) {
        self.child_pid.hangup();
        self.child_pid.reap_blocking();
        let _ = self.master_fd.close();
    }
}

impl PtySimple {
    /// Spawn `shell` under a new pseudoterminal of size `cols × rows`.
    ///
    /// # Errors
    /// Returns [`PtyError`] if any syscall fails.
    pub fn spawn(shell: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self, PtyError> {
        let win = WinSize { rows, cols, xpixel: 0, ypixel: 0 };
        let (master_fd, child_pid) = sys::forkpty(shell, args, win)?;
        master_fd.set_nonblock()?;
        Ok(Self { master_fd, child_pid })
    }

    /// Non-blocking read.
    ///
    /// Returns `Ok(None)` when no data is available yet (EAGAIN) — this is
    /// distinct from `Ok(Some(0))`, which signals a genuine EOF (the child has
    /// closed the slave side, i.e. the shell exited).
    ///
    /// # Errors
    /// Returns [`PtyError`] on real I/O errors.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, PtyError> {
        let n = self.master_fd.raw_read(buf);
        // EAGAIN (11) / EWOULDBLOCK (11 on Linux) — no data, not an error.
        if n == -11 {
            return Ok(None);
        }
        if n < 0 {
            return Err(PtyError::Exec(-n as i32));
        }
        Ok(Some(n as usize))
    }

    /// Write all bytes to the PTY master fd.
    ///
    /// # Errors
    /// Returns [`PtyError`] on I/O errors.
    pub fn write(&mut self, data: &[u8]) -> Result<(), PtyError> {
        let mut offset = 0;
        while offset < data.len() {
            let n = self.master_fd.raw_write(&data[offset..]);
            if n <= 0 {
                let errno = if n < 0 { -n as i32 } else { 5 /* EIO */ };
                return Err(PtyError::Exec(errno));
            }
            offset += n as usize;
        }
        Ok(())
    }

    /// The master fd as a plain integer, for poll(2)-based readiness waiting.
    /// The fd stays owned by this `PtySimple`; callers must only poll it.
    #[must_use]
    pub const fn master_fd(&self) -> i32 {
        self.master_fd.as_i32()
    }

    /// Push a window resize to the child process.
    ///
    /// # Errors
    /// Returns [`PtyError`] if the ioctl fails.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        let win = WinSize { rows, cols, xpixel: 0, ypixel: 0 };
        self.master_fd.set_win_size(win)
    }
}

// ── raw I/O extension trait ─────────────────────────────────────────────────

/// Non-blocking-mode setup and raw `read(2)`/`write(2)` on a PTY master fd.
///
/// Kept separate from [`PtyIoctl`] since these are plain I/O syscalls, not
/// `ioctl`s — but still belong on `RawFd` rather than as free functions.
trait PtyRawIo: Copy {
    fn set_nonblock(self) -> Result<(), PtyError>;
    fn raw_read(self, buf: &mut [u8]) -> isize;
    fn raw_write(self, buf: &[u8]) -> isize;
}

impl PtyRawIo for RawFd {
    fn set_nonblock(self) -> Result<(), PtyError> {
        use crate::sys::{FcntlCmd, OpenFlags};

        let flags = unsafe { sys::syscall2(Syscall::Fcntl, self.as_usize(), FcntlCmd::GetFl as usize) };
        if flags < 0 {
            return Err(PtyError::Exec(-flags as i32));
        }
        let ret = unsafe {
            sys::syscall3(
                Syscall::Fcntl,
                self.as_usize(),
                FcntlCmd::SetFl as usize,
                flags as usize | OpenFlags::NONBLOCK.bits(),
            )
        };
        if ret < 0 {
            return Err(PtyError::Exec(-ret as i32));
        }
        Ok(())
    }

    fn raw_read(self, buf: &mut [u8]) -> isize {
        unsafe { sys::syscall3(Syscall::Read, self.as_usize(), buf.as_mut_ptr() as usize, buf.len()) }
    }

    fn raw_write(self, buf: &[u8]) -> isize {
        unsafe { sys::syscall3(Syscall::Write, self.as_usize(), buf.as_ptr() as usize, buf.len()) }
    }
}
