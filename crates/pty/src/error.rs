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

#[cfg(test)]
mod tests {
    use super::*;
    use ququmatz::Error as UringError;

    #[test]
    fn display_includes_errno_for_simple_variants() {
        assert_eq!(PtyError::OpenPt(2).to_string(), "openpt failed: errno 2");
        assert_eq!(PtyError::Fork(12).to_string(), "fork failed: errno 12");
        assert_eq!(PtyError::Exec(2).to_string(), "exec failed: errno 2");
        assert_eq!(
            PtyError::WinSize(25).to_string(),
            "winsize ioctl failed: errno 25"
        );
        assert_eq!(
            PtyError::SignalFd(9).to_string(),
            "signalfd failed: errno 9"
        );
        assert_eq!(
            PtyError::SigMask(22).to_string(),
            "sigprocmask failed: errno 22"
        );
        assert_eq!(
            PtyError::WaitPid(10).to_string(),
            "waitpid failed: errno 10"
        );
    }

    #[test]
    fn display_sq_full_has_a_fixed_message() {
        assert_eq!(PtyError::SqFull.to_string(), "submission queue full");
    }

    #[test]
    fn display_uring_variants_delegate_to_inner_error() {
        let inner = UringError::Syscall(ququmatz::Errno::EAGAIN);
        let setup = PtyError::UringSetup(inner);
        let enter = PtyError::UringEnter(inner);
        assert_eq!(setup.to_string(), format!("io_uring setup failed: {inner}"));
        assert_eq!(enter.to_string(), format!("io_uring enter failed: {inner}"));
    }

    #[test]
    fn implements_std_error_trait() {
        // Compile-time + runtime check that `source()` doesn't panic and the
        // trait object coercion works (`Error` requires `Debug + Display`).
        let e: Box<dyn std::error::Error> = Box::new(PtyError::SqFull);
        assert!(e.source().is_none());
    }
}
