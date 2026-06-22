#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap,
         clippy::missing_const_for_fn, clippy::redundant_closure, clippy::items_after_statements,
         clippy::single_match_else, clippy::option_if_let_else, clippy::needless_return,
         clippy::map_identity, clippy::doc_markdown, clippy::unreadable_literal)]
//! Raw POSIX syscall wrappers for PTY management.
//!
//! All unsafe is contained here. Every function documents the invariant
//! that makes each unsafe block sound.

#![allow(unsafe_code)]

use crate::error::PtyError;
use ququmatz::RawFd;
use std::ffi::CString;

// ── libc constants (inline to avoid any libc dep) ─────────────────────────

// Syscall numbers (x86_64 Linux)
const SYS_CLONE: usize = 56;
const SYS_EXECVE: usize = 59;
const SYS_WAITPID: usize = 61; // actually wait4
const SYS_SIGPROCMASK: usize = 14;
const SYS_SIGNALFD4: usize = 289;
const SYS_CLOSE: usize = 3;
const SYS_IOCTL: usize = 16;
const SYS_OPEN: usize = 2;
const SYS_SETSID: usize = 112;
const SYS_DUP2: usize = 33;

// ioctl request codes
const TIOCSWINSZ: usize = 0x5414;
#[allow(dead_code)]
const TIOCGWINSZ: usize = 0x5413;
const TIOCSCTTY: usize = 0x540E;

// PTY ops
const O_RDWR: usize = 2;
const O_NOCTTY: usize = 0x400;
const O_CLOEXEC: usize = 0x80000;

// clone flags for fork
const SIGCHLD: usize = 17;

// signalfd
const SFD_NONBLOCK: usize = 0o4000;
const SFD_CLOEXEC: usize = 0o2000000;
const SIGCHLD_BIT: u64 = 1 << 16; // bit 16 in sigset (SIGCHLD = 17, 0-indexed = 16)

// wait4
const WNOHANG: usize = 1;

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

// ── raw syscall shim ───────────────────────────────────────────────────────

#[inline]
unsafe fn syscall0(nr: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as isize => ret,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

#[inline]
unsafe fn syscall1(nr: usize, a0: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as isize => ret,
        in("rdi") a0,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
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

#[inline]
unsafe fn syscall4(nr: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as isize => ret,
        in("rdi") a0,
        in("rsi") a1,
        in("rdx") a2,
        in("r10") a3,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

fn errno_to_err(ret: isize) -> Result<usize, i32> {
    if ret < 0 {
        Err((-ret) as i32)
    } else {
        Ok(ret as usize)
    }
}

// ── PTY open/fork ──────────────────────────────────────────────────────────

/// Open a PTY master fd via `/dev/ptmx`.
fn open_ptmx() -> Result<RawFd, PtyError> {
    // "/dev/ptmx\0"
    let path: &[u8] = b"/dev/ptmx\0";
    let flags = O_RDWR | O_NOCTTY | O_CLOEXEC;
    let ret = unsafe { syscall3(SYS_OPEN, path.as_ptr() as usize, flags, 0) };
    errno_to_err(ret)
        .map(|fd| RawFd::from_raw(fd))
        .map_err(PtyError::OpenPt)
}

/// Read the slave PTY device path from `/proc/self/fd/<master_fd>` symlink
/// via `ptsname`-equivalent: read `/dev/pts/<n>` by calling `TIOCGPTN` ioctl.
fn pts_path(master_fd: RawFd) -> Result<CString, PtyError> {
    // TIOCGPTN returns the pts number
    const TIOCGPTN: usize = 0x8004_5430;
    let mut pts_no: u32 = 0;
    let ret = unsafe {
        syscall3(
            SYS_IOCTL,
            master_fd.as_usize(),
            TIOCGPTN,
            core::ptr::addr_of_mut!(pts_no) as usize,
        )
    };
    errno_to_err(ret).map_err(PtyError::OpenPt)?;

    let path = std::format!("/dev/pts/{pts_no}\0");
    // SAFETY: we just formatted a valid null-terminated string.
    Ok(unsafe { CString::from_vec_with_nul_unchecked(path.into_bytes()) })
}

/// Unlock a PTY master fd (`grantpt` + `unlockpt` equivalent via `TIOCSPTLCK`).
fn unlock_pt(master_fd: RawFd) -> Result<(), PtyError> {
    const TIOCSPTLCK: usize = 0x4004_5431;
    let lock: i32 = 0;
    let ret = unsafe {
        syscall3(
            SYS_IOCTL,
            master_fd.as_usize(),
            TIOCSPTLCK,
            core::ptr::addr_of!(lock) as usize,
        )
    };
    errno_to_err(ret).map(|_| ()).map_err(PtyError::OpenPt)
}

/// Fork and exec the shell under the PTY slave.
///
/// Returns `(master_fd, child_pid)` in the parent.
///
/// # Safety
///
/// This function forks. After `fork`, only async-signal-safe functions are
/// called in the child before `execve`. We use raw syscalls throughout.
pub fn forkpty(shell: &str, args: &[&str], win_size: WinSize) -> Result<(RawFd, i32), PtyError> {
    let master_fd = open_ptmx()?;
    unlock_pt(master_fd)?;
    set_win_size(master_fd, win_size)?;
    let slave_path = pts_path(master_fd)?;

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

    // envp: inherit nothing beyond a minimal set — child inherits parent env.
    // Pass NULL as envp to keep current environment.
    let envp: *const *const u8 = core::ptr::null();

    // fork via clone(SIGCHLD, 0)
    let pid = unsafe { syscall2(SYS_CLONE, SIGCHLD, 0) };
    match errno_to_err(pid) {
        Err(e) => return Err(PtyError::Fork(e)),
        Ok(0) => {
            // ── child ──────────────────────────────────────────────────
            // Start a new session so the slave PTY becomes the controlling terminal.
            unsafe { syscall0(SYS_SETSID) };

            // Open the slave PTY.
            let flags = O_RDWR;
            let slave_fd_ret = unsafe {
                syscall3(SYS_OPEN, slave_path.as_ptr() as usize, flags, 0)
            };
            let slave_fd = match errno_to_err(slave_fd_ret) {
                Ok(fd) => fd as i32,
                Err(_) => {
                    // Can't report — just exit.
                    unsafe { syscall1(SYS_CLOSE, master_fd.as_usize()) };
                    unsafe { syscall1(crate::sys::SYS_EXIT_GROUP, 1) };
                    unreachable!()
                }
            };

            // Set the slave as the controlling terminal.
            unsafe { syscall3(SYS_IOCTL, slave_fd as usize, TIOCSCTTY, 0) };

            // Dup slave onto stdin/stdout/stderr.
            for dest in [0usize, 1, 2] {
                unsafe { syscall2(SYS_DUP2, slave_fd as usize, dest) };
            }

            // Close master and extra slave fd.
            unsafe { syscall1(SYS_CLOSE, master_fd.as_usize()) };
            if slave_fd > 2 {
                unsafe { syscall1(SYS_CLOSE, slave_fd as usize) };
            }

            // execve
            let ret = unsafe {
                syscall3(
                    SYS_EXECVE,
                    shell_cs.as_ptr() as usize,
                    argv_ptrs.as_mut_ptr() as usize,
                    envp as usize,
                )
            };
            // execve only returns on error — exit child.
            let _ = ret;
            unsafe { syscall1(SYS_EXIT_GROUP, 127) };
            unreachable!()
        }
        Ok(child_pid) => {
            // ── parent ─────────────────────────────────────────────────
            Ok((master_fd, child_pid as i32))
        }
    }
}

// syscall used only in child — referenced above
const SYS_EXIT_GROUP: usize = 231;

// ── Window size ────────────────────────────────────────────────────────────

/// Push a new window size to the PTY via `TIOCSWINSZ`.
pub fn set_win_size(master_fd: RawFd, win_size: WinSize) -> Result<(), PtyError> {
    let ret = unsafe {
        syscall3(
            SYS_IOCTL,
            master_fd.as_usize(),
            TIOCSWINSZ,
            core::ptr::addr_of!(win_size) as usize,
        )
    };
    errno_to_err(ret).map(|_| ()).map_err(PtyError::WinSize)
}

/// Query current window size via `TIOCGWINSZ`.
#[allow(dead_code)]
pub fn get_win_size(master_fd: RawFd) -> Result<WinSize, PtyError> {
    let mut ws = WinSize::default();
    let ret = unsafe {
        syscall3(
            SYS_IOCTL,
            master_fd.as_usize(),
            TIOCGWINSZ,
            core::ptr::addr_of_mut!(ws) as usize,
        )
    };
    errno_to_err(ret).map(|_| ws).map_err(PtyError::WinSize)
}

// ── signalfd ───────────────────────────────────────────────────────────────

/// Block `SIGCHLD` in the calling thread and create a `signalfd` for it.
///
/// The returned fd is readable (via io_uring) whenever a child exits.
pub fn signalfd_for_sigchld() -> Result<RawFd, PtyError> {
    // Block SIGCHLD so it doesn't interrupt syscalls.
    let mut mask = SigSet([0u64; 1]);
    mask.0[0] |= SIGCHLD_BIT;

    const SIG_BLOCK: usize = 0;
    let ret = unsafe {
        syscall4(
            SYS_SIGPROCMASK,
            SIG_BLOCK,
            core::ptr::addr_of!(mask) as usize,
            0,
            8, // sigsetsz
        )
    };
    errno_to_err(ret).map_err(PtyError::SigMask)?;

    // Create the signalfd.
    let flags = SFD_NONBLOCK | SFD_CLOEXEC;
    let ret = unsafe {
        syscall4(
            SYS_SIGNALFD4,
            usize::MAX, // -1 = new fd
            core::ptr::addr_of!(mask) as usize,
            8,     // sigsetsz
            flags,
        )
    };
    errno_to_err(ret)
        .map(|fd| RawFd::from_raw(fd))
        .map_err(PtyError::SignalFd)
}

// ── waitpid ────────────────────────────────────────────────────────────────

/// Reap a child and return its exit code.
///
/// `pid = 0` means wait for any child.
pub fn waitpid(pid: i32) -> Result<i32, PtyError> {
    let mut status: i32 = 0;
    let ret = unsafe {
        syscall4(
            SYS_WAITPID,
            pid as usize,
            core::ptr::addr_of_mut!(status) as usize,
            WNOHANG,
            0,
        )
    };
    errno_to_err(ret).map_err(PtyError::WaitPid)?;
    // Decode exit status: WEXITSTATUS(status)
    Ok((status >> 8) & 0xff)
}

// ── fd helpers ─────────────────────────────────────────────────────────────

/// Close a raw fd.
pub fn close_fd(fd: RawFd) -> Result<(), i32> {
    let ret = unsafe { syscall1(SYS_CLOSE, fd.as_usize()) };
    errno_to_err(ret).map(|_| ()).map_err(|e| e)
}
