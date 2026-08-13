//! Real descriptors for the FD-passing tests, plus a liveness probe.
//!
//! Descriptor lifetime is a correctness property of this module, so the
//! tests assert on actual kernel state rather than on a count this crate
//! keeps for itself. `/dev/null` is the source: opening it is cheap, always
//! available, and yields a descriptor nothing else in the process owns.

use std::os::fd::{IntoRawFd, OwnedFd, RawFd};
use std::sync::{Mutex, MutexGuard, OnceLock};

unsafe extern "C" {
    fn fcntl(fd: RawFd, cmd: i32, arg: i32) -> i32;
}

/// Serializes every test that observes process-global descriptor state.
///
/// Descriptor numbers and `/proc/self/fd` belong to the whole process, but
/// cargo runs tests on parallel threads. Without this lock a sibling test's
/// `open` reuses the number this one just proved closed, and a baseline
/// count taken before another test's sockets exist never comes back. Both
/// produce failures that look like leaks and are not.
static FD_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Held for the body of a descriptor-lifetime test.
pub struct FdTestGuard {
    _lock: MutexGuard<'static, ()>,
}

impl FdTestGuard {
    /// Take the process-wide descriptor-test lock.
    ///
    /// A panicking test poisons the mutex; the poison is stepped over
    /// deliberately, because the lock guards test *isolation*, not any
    /// invariant of the data behind it.
    #[must_use]
    pub fn acquire() -> Self {
        let lock = FD_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self { _lock: lock }
    }
}

/// `F_GETFD` — query a descriptor's flags. Fails with `EBADF` when the
/// descriptor is closed, which is exactly the liveness question the
/// lifetime tests ask.
const F_GETFD: i32 = 1;

/// `FD_CLOEXEC` — the flag `MSG_CMSG_CLOEXEC` sets on a received descriptor.
const FD_CLOEXEC: i32 = 1;

/// A batch of open descriptors the caller can convert into [`OwnedFd`]s or
/// borrow, while remembering the raw numbers so a test can ask the kernel
/// whether they were really closed.
pub struct LoanedFds {
    raw: Vec<RawFd>,
}

impl LoanedFds {
    /// Open `count` fresh descriptors on `/dev/null`.
    #[must_use]
    pub fn open(count: usize) -> Self {
        let raw = (0..count)
            .map(|_| {
                std::fs::File::open("/dev/null")
                    .expect("/dev/null must be openable")
                    .into_raw_fd()
            })
            .collect();
        Self { raw }
    }

    /// The raw descriptor numbers, for a later [`Self::is_open`] probe.
    #[must_use]
    pub fn raw_numbers(&self) -> Vec<RawFd> {
        self.raw.clone()
    }

    /// Hand ownership of every descriptor to the caller.
    #[must_use]
    pub fn into_owned(self) -> Vec<OwnedFd> {
        use std::os::fd::FromRawFd;
        self.raw
            .iter()
            // SAFETY: each number came from a `File` that released
            // ownership, and this consumes `self`, so ownership transfers
            // exactly once.
            .map(|fd| unsafe { OwnedFd::from_raw_fd(*fd) })
            .collect()
    }

    /// Ask the kernel whether `fd` is still an open descriptor in this
    /// process.
    #[must_use]
    pub fn is_open(fd: RawFd) -> bool {
        // SAFETY: F_GETFD only reads flags. An invalid fd returns -1 with
        // EBADF rather than doing anything unsound, which is the answer we
        // want.
        unsafe { fcntl(fd, F_GETFD, 0) != -1 }
    }

    /// Is `fd` marked close-on-exec?
    ///
    /// Asks the kernel rather than trusting the flag this crate passed to
    /// `recvmsg`, so the test proves the descriptor's actual state.
    #[must_use]
    pub fn is_cloexec(fd: RawFd) -> bool {
        // SAFETY: F_GETFD only reads flags from a descriptor we own.
        let flags = unsafe { fcntl(fd, F_GETFD, 0) };
        flags >= 0 && flags & FD_CLOEXEC == FD_CLOEXEC
    }

    /// How many descriptors this process currently has open.
    ///
    /// Counts the entries in `/proc/self/fd`, which is the kernel's own
    /// view. A leak test that counted only what this crate tracked would
    /// pass even while descriptors leaked.
    #[must_use]
    pub fn process_open_count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .expect("/proc must be mounted")
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/dev/null` is opened through `File`, which sets close-on-exec
    /// itself, so this pins the probe against a descriptor of known state.
    #[test]
    fn the_cloexec_probe_reads_a_real_descriptor_flag() {
        let _guard = FdTestGuard::acquire();
        let loaned = LoanedFds::open(1);
        let raw = loaned.raw_numbers()[0];
        assert!(LoanedFds::is_cloexec(raw));
        drop(loaned.into_owned());
        assert!(!LoanedFds::is_cloexec(raw), "a closed fd has no flags");
    }

    #[test]
    fn a_loaned_descriptor_is_open_and_closes_with_its_owner() {
        let _guard = FdTestGuard::acquire();
        let loaned = LoanedFds::open(1);
        let raw = loaned.raw_numbers()[0];
        assert!(LoanedFds::is_open(raw));
        drop(loaned.into_owned());
        assert!(!LoanedFds::is_open(raw));
    }

    #[test]
    fn opening_descriptors_raises_the_process_count() {
        let _guard = FdTestGuard::acquire();
        let before = LoanedFds::process_open_count();
        let loaned = LoanedFds::open(4);
        let during = LoanedFds::process_open_count();
        assert!(during >= before + 4);
        drop(loaned.into_owned());
        assert_eq!(LoanedFds::process_open_count(), before);
    }
}
