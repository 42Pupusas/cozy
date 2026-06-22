#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
//! A dead-simple synchronous PTY wrapper for the M4 integration prototype.
//!
//! Uses raw `read(2)` / `write(2)` syscalls (non-blocking) instead of
//! `io_uring`, so it can be driven from a single event-loop thread without
//! spawning a PTY thread. Replaced by `Pty` + ring buffers in M5.

use crate::error::PtyError;
use crate::sys::{self, WinSize};
use ququmatz::RawFd;

/// A simple blocking-ish PTY handle for the integration prototype.
pub struct PtySimple {
    master_fd: RawFd,
    #[allow(dead_code)]
    child_pid: i32,
}

impl PtySimple {
    /// Spawn `shell` under a new pseudoterminal of size `cols × rows`.
    ///
    /// # Errors
    /// Returns [`PtyError`] if any syscall fails.
    pub fn spawn(shell: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self, PtyError> {
        let win = WinSize { rows, cols, xpixel: 0, ypixel: 0 };
        let (master_fd, child_pid) = sys::forkpty(shell, args, win)?;
        set_nonblock(master_fd)?;
        Ok(Self { master_fd, child_pid })
    }

    /// Non-blocking read.  Returns `Ok(0)` when no data is available (EAGAIN).
    ///
    /// # Errors
    /// Returns [`PtyError`] on real I/O errors.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<usize, PtyError> {
        let n = raw_read(self.master_fd, buf);
        // EAGAIN (11) / EWOULDBLOCK (11 on Linux) — no data, not an error.
        if n == -11 {
            return Ok(0);
        }
        if n < 0 {
            return Err(PtyError::Exec(-n as i32));
        }
        Ok(n as usize)
    }

    /// Write all bytes to the PTY master fd.
    ///
    /// # Errors
    /// Returns [`PtyError`] on I/O errors.
    pub fn write(&mut self, data: &[u8]) -> Result<(), PtyError> {
        let mut offset = 0;
        while offset < data.len() {
            let n = raw_write(self.master_fd, &data[offset..]);
            if n <= 0 {
                let errno = if n < 0 { -n as i32 } else { 5 /* EIO */ };
                return Err(PtyError::Exec(errno));
            }
            offset += n as usize;
        }
        Ok(())
    }

    /// Push a window resize to the child process.
    ///
    /// # Errors
    /// Returns [`PtyError`] if the ioctl fails.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        let win = WinSize { rows, cols, xpixel: 0, ypixel: 0 };
        sys::set_win_size(self.master_fd, win)
    }
}

// ── raw syscalls ─────────────────────────────────────────────────────────────

fn set_nonblock(fd: RawFd) -> Result<(), PtyError> {
    const SYS_FCNTL: usize = 72;
    const F_GETFL: usize = 3;
    const F_SETFL: usize = 4;
    const O_NONBLOCK: usize = 0o4000;
    let flags = unsafe { syscall2(SYS_FCNTL, fd.as_usize(), F_GETFL) };
    if flags < 0 {
        return Err(PtyError::Exec(-flags as i32));
    }
    let ret = unsafe {
        syscall3(SYS_FCNTL, fd.as_usize(), F_SETFL, flags as usize | O_NONBLOCK)
    };
    if ret < 0 {
        return Err(PtyError::Exec(-ret as i32));
    }
    Ok(())
}

fn raw_read(fd: RawFd, buf: &mut [u8]) -> isize {
    const SYS_READ: usize = 0;
    unsafe { syscall3(SYS_READ, fd.as_usize(), buf.as_mut_ptr() as usize, buf.len()) }
}

fn raw_write(fd: RawFd, buf: &[u8]) -> isize {
    const SYS_WRITE: usize = 1;
    unsafe { syscall3(SYS_WRITE, fd.as_usize(), buf.as_ptr() as usize, buf.len()) }
}

#[inline]
unsafe fn syscall2(nr: usize, a0: usize, a1: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as isize => ret,
        in("rdi") a0,
        in("rsi") a1,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

#[inline]
unsafe fn syscall3(nr: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as isize => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}
