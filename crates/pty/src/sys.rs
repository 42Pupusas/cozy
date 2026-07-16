// Raw syscall FFI: `as` casts between C int widths are bounded and correct.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! Raw POSIX syscall wrappers for PTY management.
//!
//! All unsafe is contained here. Every function documents the invariant
//! that makes each unsafe block sound.
//!
//! Syscall numbers, `ioctl` requests, and flag bits are grouped into small
//! `#[repr(usize)]` enums / bitflag-ish newtypes instead of loose `const`s,
//! so a bad value is a type error rather than a typo in a sea of `usize`s.
//! Each enum is `#[repr(usize)]`, so a plain `as usize` cast at the call
//! site is all that's needed to feed it to a syscall — no wrapper method.
//! Operations on an open fd (window size, locking, `TIOCSCTTY`, closing) are
//! exposed as extension traits ([`PtyIoctl`], [`CloseFd`]) on [`RawFd`]
//! rather than free functions taking the fd as their first argument.

#![allow(unsafe_code)]

use crate::error::PtyError;
use ququmatz::RawFd;
use std::ffi::CString;

// ── syscall numbers (x86_64 Linux) ─────────────────────────────────────────

/// `x86_64` Linux syscall numbers used by this crate.
///
/// `pty` only targets `Linux/x86_64` (see the crate-level doc comment), so a
/// single flat enum is sufficient — no per-arch tables needed.
#[cfg(target_arch = "x86_64")]
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syscall {
    Read = 0,
    Write = 1,
    Open = 2,
    Close = 3,
    SigProcMask = 14,
    Ioctl = 16,
    Dup2 = 33,
    Clone = 56,
    Execve = 59,
    /// Also known as `wait4`.
    Wait4 = 61,
    Kill = 62,
    Fcntl = 72,
    SetSid = 112,
    SignalFd4 = 289,
    ExitGroup = 231,
}

/// A POSIX signal number.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Hup = 1,
    Chld = 17,
}

/// `ioctl(2)` request codes used to manage a PTY.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoctlRequest {
    /// `TIOCGPTN` — fetch the slave PTY number.
    GetPtyNumber = 0x8004_5430,
    /// `TIOCSPTLCK` — lock/unlock the slave PTY (0 = unlock).
    SetPtyLock = 0x4004_5431,
    /// `TIOCGWINSZ` — read the window size.
    GetWinSize = 0x5413,
    /// `TIOCSWINSZ` — set the window size.
    SetWinSize = 0x5414,
    /// `TIOCSCTTY` — make this fd the controlling terminal.
    SetControllingTty = 0x540E,
}

/// `fcntl(2)` commands.
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FcntlCmd {
    GetFl = 3,
    SetFl = 4,
}

/// `sigprocmask(2)` "how" argument.
#[repr(usize)]
#[derive(Clone, Copy)]
pub enum SigProcMaskHow {
    Block = 0,
}

/// `wait4(2)` option flags.
#[repr(usize)]
#[derive(Clone, Copy)]
pub enum Wait4Flags {
    Blocking = 0,
    NoHang = 1,
}

/// `open(2)` / `fcntl(2)` flag bits.
///
/// A plain enum can't be OR-ed together, so this is a bitflag-style newtype
/// with associated consts and a `BitOr` impl — the minimal amount of
/// ceremony needed to keep flag combination type-safe.
#[derive(Clone, Copy)]
pub struct OpenFlags(usize);

impl OpenFlags {
    pub const RDWR: Self = Self(2);
    pub const NOCTTY: Self = Self(0x400);
    pub const CLOEXEC: Self = Self(0x8_0000);
    pub const NONBLOCK: Self = Self(0o4000);

    pub const fn bits(self) -> usize {
        self.0
    }
}

impl core::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// `signalfd4(2)` flag bits.
#[derive(Clone, Copy)]
struct SignalFdFlags(usize);

impl SignalFdFlags {
    const NONBLOCK: Self = Self(0o4000);
    const CLOEXEC: Self = Self(0o2_000_000);

    const fn bits(self) -> usize {
        self.0
    }
}

impl core::ops::BitOr for SignalFdFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Terminal window dimensions.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct WinSize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

/// Sigset large enough for a full 64-signal mask (Linux uses 8 bytes).
#[repr(C)]
struct SigSet([u64; 1]);

impl SigSet {
    const fn empty() -> Self {
        Self([0u64; 1])
    }

    const fn insert(&mut self, sig: Signal) {
        // Sigset bit N (0-indexed) corresponds to signal N+1.
        self.0[0] |= 1u64 << (sig as usize - 1);
    }
}

// ── raw syscall shim ───────────────────────────────────────────────────────

impl Syscall {
    /// Invoke this syscall with no arguments.
    ///
    /// # Safety
    /// Caller upholds the unsafe-fn contract for whichever syscall `self`
    /// names (valid arguments, valid pointers, etc. per that syscall's man
    /// page).
    #[inline]
    unsafe fn invoke0(self) -> isize {
        let ret: isize;
        // SAFETY: caller upholds the contract documented above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") self as isize => ret,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }

    /// Invoke this syscall with one argument.
    ///
    /// # Safety
    /// See [`Syscall::invoke0`].
    #[inline]
    unsafe fn invoke1(self, a0: usize) -> isize {
        let ret: isize;
        // SAFETY: caller upholds the contract documented above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") self as isize => ret,
                in("rdi") a0,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }

    /// Invoke this syscall with two arguments.
    ///
    /// # Safety
    /// See [`Syscall::invoke0`].
    #[inline]
    pub unsafe fn invoke2(self, a0: usize, a1: usize) -> isize {
        let ret: isize;
        // SAFETY: caller upholds the contract documented above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") self as isize => ret,
                in("rdi") a0,
                in("rsi") a1,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }

    /// Invoke this syscall with three arguments.
    ///
    /// # Safety
    /// See [`Syscall::invoke0`].
    #[inline]
    pub unsafe fn invoke3(self, a0: usize, a1: usize, a2: usize) -> isize {
        let ret: isize;
        // SAFETY: caller upholds the contract documented above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") self as isize => ret,
                in("rdi") a0,
                in("rsi") a1,
                in("rdx") a2,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }

    /// Invoke this syscall with four arguments.
    ///
    /// # Safety
    /// See [`Syscall::invoke0`].
    #[inline]
    unsafe fn invoke4(self, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
        let ret: isize;
        // SAFETY: caller upholds the contract documented above.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") self as isize => ret,
                in("rdi") a0,
                in("rsi") a1,
                in("rdx") a2,
                in("r10") a3,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        ret
    }
}

const fn errno_to_err(ret: isize) -> Result<usize, i32> {
    if ret < 0 {
        Err((-ret) as i32)
    } else {
        Ok(ret as usize)
    }
}

// ── fd extension traits ─────────────────────────────────────────────────────

/// PTY-related `ioctl(2)` operations on a raw fd.
///
/// Grouping these as an extension trait on [`RawFd`] (rather than five free
/// functions each taking a fd) reads as `fd.set_win_size(..)` at the call
/// site and keeps every PTY ioctl in one place.
pub trait PtyIoctl: Copy {
    /// Fetch the slave PTY number (`TIOCGPTN`) — used to build `/dev/pts/<n>`.
    ///
    /// # Errors
    /// Returns [`PtyError::OpenPt`] if the ioctl fails.
    fn pty_number(self) -> Result<u32, PtyError>;

    /// Unlock the slave PTY (`TIOCSPTLCK` with lock = 0), the raw-syscall
    /// equivalent of `unlockpt(3)`.
    ///
    /// # Errors
    /// Returns [`PtyError::OpenPt`] if the ioctl fails.
    fn unlock_pty(self) -> Result<(), PtyError>;

    /// Push a new window size via `TIOCSWINSZ`.
    ///
    /// # Errors
    /// Returns [`PtyError::WinSize`] if the ioctl fails.
    fn set_win_size(self, win_size: WinSize) -> Result<(), PtyError>;

    /// Query the current window size via `TIOCGWINSZ`.
    ///
    /// # Errors
    /// Returns [`PtyError::WinSize`] if the ioctl fails.
    fn get_win_size(self) -> Result<WinSize, PtyError>;

    /// Make this fd the controlling terminal of the calling process
    /// (`TIOCSCTTY`). Only meaningful post-fork, in a new session.
    ///
    /// # Errors
    /// Returns [`PtyError::OpenPt`] if the ioctl fails.
    fn set_controlling_terminal(self) -> Result<(), PtyError>;
}

impl PtyIoctl for RawFd {
    fn pty_number(self) -> Result<u32, PtyError> {
        let mut pts_no: u32 = 0;
        let ret = unsafe {
            Syscall::Ioctl.invoke3(
                self.as_usize(),
                IoctlRequest::GetPtyNumber as usize,
                core::ptr::addr_of_mut!(pts_no) as usize,
            )
        };
        errno_to_err(ret).map(|_| pts_no).map_err(PtyError::OpenPt)
    }

    fn unlock_pty(self) -> Result<(), PtyError> {
        let lock: i32 = 0;
        let ret = unsafe {
            Syscall::Ioctl.invoke3(
                self.as_usize(),
                IoctlRequest::SetPtyLock as usize,
                core::ptr::addr_of!(lock) as usize,
            )
        };
        errno_to_err(ret).map(|_| ()).map_err(PtyError::OpenPt)
    }

    fn set_win_size(self, win_size: WinSize) -> Result<(), PtyError> {
        let ret = unsafe {
            Syscall::Ioctl.invoke3(
                self.as_usize(),
                IoctlRequest::SetWinSize as usize,
                core::ptr::addr_of!(win_size) as usize,
            )
        };
        errno_to_err(ret).map(|_| ()).map_err(PtyError::WinSize)
    }

    #[allow(dead_code)] // exposed for completeness; no current caller queries winsize
    fn get_win_size(self) -> Result<WinSize, PtyError> {
        let mut ws = WinSize::default();
        let ret = unsafe {
            Syscall::Ioctl.invoke3(
                self.as_usize(),
                IoctlRequest::GetWinSize as usize,
                core::ptr::addr_of_mut!(ws) as usize,
            )
        };
        errno_to_err(ret).map(|_| ws).map_err(PtyError::WinSize)
    }

    fn set_controlling_terminal(self) -> Result<(), PtyError> {
        let ret = unsafe {
            Syscall::Ioctl.invoke3(self.as_usize(), IoctlRequest::SetControllingTty as usize, 0)
        };
        errno_to_err(ret).map(|_| ()).map_err(PtyError::OpenPt)
    }
}

/// Close a raw fd via a direct syscall (no libc dependency).
pub trait CloseFd {
    /// # Errors
    /// Returns the raw errno if `close(2)` fails.
    fn close(self) -> Result<(), i32>;
}

impl CloseFd for RawFd {
    fn close(self) -> Result<(), i32> {
        let ret = unsafe { Syscall::Close.invoke1(self.as_usize()) };
        errno_to_err(ret).map(|_| ())
    }
}

// ── child process handle ────────────────────────────────────────────────────

/// PID of a child spawned under a PTY slave.
///
/// Wrapping the raw `i32` keeps `hangup` / `reap_blocking` / `wait_nohang`
/// as methods on the process itself instead of free functions each
/// re-stating "this takes a pid".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildPid(pub i32);

impl ChildPid {
    /// Send `SIGHUP` (best effort — used on teardown so the shell doesn't
    /// outlive us as an orphan holding the PTY slave open).
    pub fn hangup(self) {
        if self.0 > 0 {
            unsafe { Syscall::Kill.invoke2(self.0 as usize, Signal::Hup as usize) };
        }
    }

    /// Blocking reap (no `WNOHANG`) — call after [`ChildPid::hangup`] so the
    /// zombie is collected and we don't leak a process-table entry.
    pub fn reap_blocking(self) {
        if self.0 <= 0 {
            return;
        }
        let mut status: i32 = 0;
        unsafe {
            Syscall::Wait4.invoke4(
                self.0 as usize,
                core::ptr::addr_of_mut!(status) as usize,
                Wait4Flags::Blocking as usize,
                0,
            )
        };
    }

    /// Non-blocking reap (`WNOHANG`).
    ///
    /// `wait4`'s return value distinguishes "nothing to reap yet" (returns
    /// `0`) from "reaped this pid" (returns the pid) -- collapsing that into
    /// a bare exit-code `i32` would silently misreport a still-running child
    /// as "exited with code 0". [`WaitStatus`] keeps that distinction, plus
    /// whether the child exited normally vs. died by signal.
    ///
    /// # Errors
    /// Returns [`PtyError::WaitPid`] if `wait4` fails.
    pub fn wait_nohang(self) -> Result<WaitStatus, PtyError> {
        let mut status: i32 = 0;
        let ret = unsafe {
            Syscall::Wait4.invoke4(
                self.0 as usize,
                core::ptr::addr_of_mut!(status) as usize,
                Wait4Flags::NoHang as usize,
                0,
            )
        };
        let reaped_pid = errno_to_err(ret).map_err(PtyError::WaitPid)?;
        if reaped_pid == 0 {
            return Ok(WaitStatus::StillRunning);
        }
        Ok(decode_wait_status(status))
    }
}

/// Decode a raw `wait4(2)` status word (glibc's `<bits/waitstatus.h>` layout)
/// into a [`WaitStatus`]. Pulled out of [`ChildPid::wait_nohang`] as a pure
/// function so the WIFEXITED/WEXITSTATUS/WTERMSIG bit math is unit-testable
/// without forking a real child.
const fn decode_wait_status(status: i32) -> WaitStatus {
    // WIFEXITED(status): low 7 bits of status are 0 for a normal exit.
    #[allow(clippy::verbose_bit_mask)] // & 0x7f reads as WIFEXITED's mask, not a power-of-two check
    if status & 0x7f == 0 {
        // WEXITSTATUS(status)
        WaitStatus::Exited((status >> 8) & 0xff)
    } else {
        // WTERMSIG(status)
        WaitStatus::Signaled(status & 0x7f)
    }
}

/// Outcome of a non-blocking [`ChildPid::wait_nohang`] reap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStatus {
    /// The child has not exited yet (`wait4` returned 0 under `WNOHANG`).
    StillRunning,
    /// The child exited normally with this exit code.
    Exited(i32),
    /// The child was killed by this signal number.
    Signaled(i32),
}

// ── PTY open/fork ──────────────────────────────────────────────────────────

/// Constructor namespace for spawning a PTY + child process.
///
/// A zero-sized marker type rather than bare `pub fn`s at module scope:
/// there's no existing `RawFd`/`ChildPid` receiver to hang these on (they
/// produce one), so the idiomatic shape is `RawPty::fork(..)` -- the same
/// pattern as `std::fs::File::open` or `TcpStream::connect`, which are
/// associated functions rather than free functions or trait methods on an
/// unrelated type.
pub struct RawPty;

impl RawPty {
    /// Open a PTY master fd via `/dev/ptmx`.
    fn open_ptmx() -> Result<RawFd, PtyError> {
        // "/dev/ptmx\0"
        let path: &[u8] = b"/dev/ptmx\0";
        let flags = OpenFlags::RDWR | OpenFlags::NOCTTY | OpenFlags::CLOEXEC;
        let ret = unsafe { Syscall::Open.invoke3(path.as_ptr() as usize, flags.bits(), 0) };
        errno_to_err(ret)
            .map(RawFd::from_raw)
            .map_err(PtyError::OpenPt)
    }

    /// Read the slave PTY device path for `master_fd` (`ptsname`-equivalent,
    /// via the `TIOCGPTN` ioctl behind [`PtyIoctl::pty_number`]).
    fn pts_path(master_fd: RawFd) -> Result<CString, PtyError> {
        let pts_no = master_fd.pty_number()?;
        let path = std::format!("/dev/pts/{pts_no}\0");
        // SAFETY: we just formatted a valid null-terminated string.
        Ok(unsafe { CString::from_vec_with_nul_unchecked(path.into_bytes()) })
    }

    /// Fork and exec the shell under the PTY slave.
    ///
    /// Returns `(master_fd, child_pid)` in the parent.
    ///
    /// # Safety
    ///
    /// This function forks. After `fork`, only async-signal-safe functions
    /// are called in the child before `execve`. We use raw syscalls
    /// throughout.
    pub fn fork(
        shell: &str,
        args: &[&str],
        win_size: WinSize,
    ) -> Result<(RawFd, ChildPid), PtyError> {
        let master_fd = Self::open_ptmx()?;
        master_fd.unlock_pty()?;
        master_fd.set_win_size(win_size)?;
        let slave_path = Self::pts_path(master_fd)?;

        // Build execve argv.
        let shell_cs = CString::new(shell).map_err(|_| PtyError::Exec(22))?;
        let args_cs: Vec<CString> = args
            .iter()
            .map(|a| CString::new(*a).map_err(|_| PtyError::Exec(22)))
            .collect::<Result<_, _>>()?;

        // argv array: shell_cs + args_cs + NULL
        let mut argv_ptrs: Vec<*const u8> = std::iter::once(shell_cs.as_ptr().cast())
            .chain(args_cs.iter().map(|cs| cs.as_ptr().cast()))
            .chain(std::iter::once(core::ptr::null()))
            .collect();

        // envp: a raw execve does NOT inherit the parent environment — passing
        // NULL gives the child an *empty* env (no TERM, no PATH), which makes
        // programs like `cargo` and `ls` disable colour. So we build the child
        // environment explicitly: every parent variable, with TERM/COLORTERM
        // guaranteed.
        //
        // The `CString`s must outlive the `execve` below; we keep them in
        // `env_cs` and the pointer array in `envp_ptrs` (both live until
        // function return, which never happens in the child — it execs or
        // exits). Built *before* the fork so the child only dereferences
        // already-constructed pointers.
        let mut have_term = false;
        let mut have_colorterm = false;
        let mut env_cs: Vec<CString> = Vec::new();
        for (k, v) in std::env::vars() {
            if k == "TERM" {
                have_term = true;
            }
            if k == "COLORTERM" {
                have_colorterm = true;
            }
            if let Ok(cs) = CString::new(format!("{k}={v}")) {
                env_cs.push(cs);
            }
        }
        if !have_term {
            env_cs.push(CString::new("TERM=xterm-256color").unwrap());
        }
        if !have_colorterm {
            env_cs.push(CString::new("COLORTERM=truecolor").unwrap());
        }
        let envp_ptrs: Vec<*const u8> = env_cs
            .iter()
            .map(|cs| cs.as_ptr().cast())
            .chain(std::iter::once(core::ptr::null()))
            .collect();
        let envp: *const *const u8 = envp_ptrs.as_ptr();

        // fork via clone(SIGCHLD, 0)
        let pid = unsafe { Syscall::Clone.invoke2(Signal::Chld as usize, 0) };
        match errno_to_err(pid) {
            Err(e) => Err(PtyError::Fork(e)),
            Ok(0) => {
                // ── child ──────────────────────────────────────────────
                // Start a new session so the slave PTY becomes the
                // controlling terminal.
                unsafe { Syscall::SetSid.invoke0() };

                // Open the slave PTY.
                let slave_fd_ret = unsafe {
                    Syscall::Open.invoke3(slave_path.as_ptr() as usize, OpenFlags::RDWR.bits(), 0)
                };
                // Post-fork child: an `if let` with a diverging else is
                // clearer than `map_or_else` with side-effecting closures.
                #[allow(clippy::option_if_let_else)]
                let slave_fd = if let Ok(fd) = errno_to_err(slave_fd_ret) {
                    RawFd::from_raw(fd)
                } else {
                    // Can't report -- just exit.
                    unsafe { Syscall::Close.invoke1(master_fd.as_usize()) };
                    unsafe { Syscall::ExitGroup.invoke1(1) };
                    unreachable!()
                };

                // Set the slave as the controlling terminal. If this fails
                // the shell starts with no controlling tty -- no job
                // control, Ctrl-C/Ctrl-Z don't work -- which is bad enough
                // to abort the child rather than limp on silently.
                if slave_fd.set_controlling_terminal().is_err() {
                    unsafe { Syscall::Close.invoke1(master_fd.as_usize()) };
                    unsafe { Syscall::Close.invoke1(slave_fd.as_usize()) };
                    unsafe { Syscall::ExitGroup.invoke1(1) };
                    unreachable!()
                }

                // Dup slave onto stdin/stdout/stderr.
                for dest in [0usize, 1, 2] {
                    unsafe { Syscall::Dup2.invoke2(slave_fd.as_usize(), dest) };
                }

                // Close master and extra slave fd.
                unsafe { Syscall::Close.invoke1(master_fd.as_usize()) };
                if slave_fd.as_i32() > 2 {
                    unsafe { Syscall::Close.invoke1(slave_fd.as_usize()) };
                }

                // execve only returns on error -- exit child. There is
                // nowhere left to report the errno (we're past the point of
                // no return, stdio is already the slave), so a fixed exit
                // code is genuinely the best we can do here, matching the
                // shell convention of 127 for "command not found/exec
                // failed".
                unsafe {
                    Syscall::Execve.invoke3(
                        shell_cs.as_ptr() as usize,
                        argv_ptrs.as_mut_ptr() as usize,
                        envp as usize,
                    )
                };
                unsafe { Syscall::ExitGroup.invoke1(127) };
                unreachable!()
            }
            Ok(child_pid) => {
                // ── parent ─────────────────────────────────────────────
                Ok((master_fd, ChildPid(child_pid as i32)))
            }
        }
    }

    /// Block `SIGCHLD` in the calling thread and create a `signalfd` for it.
    ///
    /// The returned fd is readable (via `io_uring`) whenever a child exits.
    pub fn signalfd_for_sigchld() -> Result<RawFd, PtyError> {
        // Block SIGCHLD so it doesn't interrupt syscalls.
        let mut mask = SigSet::empty();
        mask.insert(Signal::Chld);

        let ret = unsafe {
            Syscall::SigProcMask.invoke4(
                SigProcMaskHow::Block as usize,
                core::ptr::addr_of!(mask) as usize,
                0,
                8, // sigsetsz
            )
        };
        errno_to_err(ret).map_err(PtyError::SigMask)?;

        // Create the signalfd.
        let flags = SignalFdFlags::NONBLOCK | SignalFdFlags::CLOEXEC;
        let ret = unsafe {
            Syscall::SignalFd4.invoke4(
                usize::MAX, // -1 = new fd
                core::ptr::addr_of!(mask) as usize,
                8, // sigsetsz
                flags.bits(),
            )
        };
        errno_to_err(ret)
            .map(RawFd::from_raw)
            .map_err(PtyError::SignalFd)
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenFlags, SigSet, Signal, WaitStatus, decode_wait_status, errno_to_err};

    #[test]
    fn wait_status_normal_exit_zero() {
        assert_eq!(decode_wait_status(0), WaitStatus::Exited(0));
    }

    #[test]
    fn wait_status_normal_exit_nonzero() {
        assert_eq!(decode_wait_status(42 << 8), WaitStatus::Exited(42));
    }

    #[test]
    fn wait_status_exit_code_255() {
        assert_eq!(decode_wait_status(255 << 8), WaitStatus::Exited(255));
    }

    #[test]
    fn wait_status_killed_by_sigkill() {
        assert_eq!(decode_wait_status(9), WaitStatus::Signaled(9));
    }

    #[test]
    fn wait_status_killed_by_sigsegv() {
        assert_eq!(decode_wait_status(11), WaitStatus::Signaled(11));
    }

    #[test]
    fn wait_status_high_bits_ignored_for_signal_number() {
        assert_eq!(decode_wait_status(0x80 | 11), WaitStatus::Signaled(11));
    }

    #[test]
    fn wait_status_exit_status_masked_to_8_bits() {
        assert_eq!(decode_wait_status(0x1FF00), WaitStatus::Exited(0xFF));
    }

    #[test]
    fn open_flags_or_combines_bits() {
        let combined = OpenFlags::RDWR | OpenFlags::NOCTTY | OpenFlags::CLOEXEC;
        assert_eq!(
            combined.bits(),
            OpenFlags::RDWR.bits() | OpenFlags::NOCTTY.bits() | OpenFlags::CLOEXEC.bits()
        );
    }

    #[test]
    fn open_flags_single_flag_roundtrips() {
        assert_eq!(OpenFlags::NONBLOCK.bits(), 0o4000);
    }

    #[test]
    fn sigset_empty_has_no_bits() {
        assert_eq!(SigSet::empty().0[0], 0);
    }

    #[test]
    fn sigset_insert_chld_sets_correct_bit() {
        let mut set = SigSet::empty();
        set.insert(Signal::Chld);
        assert_eq!(set.0[0], 1u64 << 16);
    }

    #[test]
    fn sigset_insert_hup_sets_correct_bit() {
        let mut set = SigSet::empty();
        set.insert(Signal::Hup);
        assert_eq!(set.0[0], 1u64);
    }

    #[test]
    fn sigset_insert_multiple_signals_ors_bits() {
        let mut set = SigSet::empty();
        set.insert(Signal::Hup);
        set.insert(Signal::Chld);
        assert_eq!(set.0[0], 1u64 | (1u64 << 16));
    }

    #[test]
    fn errno_to_err_positive_is_ok() {
        assert_eq!(errno_to_err(42), Ok(42));
    }

    #[test]
    fn errno_to_err_zero_is_ok() {
        assert_eq!(errno_to_err(0), Ok(0));
    }

    #[test]
    fn errno_to_err_negative_becomes_positive_errno() {
        assert_eq!(errno_to_err(-11), Err(11));
    }

    #[test]
    fn errno_to_err_negative_one() {
        assert_eq!(errno_to_err(-1), Err(1));
    }
}
