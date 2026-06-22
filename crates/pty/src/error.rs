//! PTY error type.

use ququmatz::Error as UringError;

/// Errors that can occur during PTY setup or operation.
#[derive(Debug)]
pub enum PtyError {
    /// `posix_openpt` / `grantpt` / `unlockpt` / `ptsname` failed.
    OpenPt(i32),
    /// `fork` syscall failed.
    Fork(i32),
    /// `execve` in the child failed (errno propagated via pipe).
    Exec(i32),
    /// `TIOCSWINSZ` / `TIOCGWINSZ` ioctl failed.
    WinSize(i32),
    /// `signalfd` syscall failed.
    SignalFd(i32),
    /// `sigprocmask` syscall failed.
    SigMask(i32),
    /// `io_uring_setup` failed.
    UringSetup(UringError),
    /// `io_uring_enter` failed.
    UringEnter(UringError),
    /// Submission queue was unexpectedly full.
    SqFull,
    /// `waitpid` returned an unexpected value.
    WaitPid(i32),
}

impl core::fmt::Display for PtyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OpenPt(e) => write!(f, "openpt failed: errno {e}"),
            Self::Fork(e) => write!(f, "fork failed: errno {e}"),
            Self::Exec(e) => write!(f, "exec failed: errno {e}"),
            Self::WinSize(e) => write!(f, "winsize ioctl failed: errno {e}"),
            Self::SignalFd(e) => write!(f, "signalfd failed: errno {e}"),
            Self::SigMask(e) => write!(f, "sigprocmask failed: errno {e}"),
            Self::UringSetup(e) => write!(f, "io_uring setup failed: {e}"),
            Self::UringEnter(e) => write!(f, "io_uring enter failed: {e}"),
            Self::SqFull => write!(f, "submission queue full"),
            Self::WaitPid(e) => write!(f, "waitpid failed: errno {e}"),
        }
    }
}

impl std::error::Error for PtyError {}
