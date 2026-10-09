//! A tiny cross-thread wakeup primitive built on `eventfd(2)` + `poll(2)`.
//!
//! The lock-free SPSC rings (`quetzalcoatl`) block via `std::thread::park`,
//! which is *thread-targeted*: a consumer can `pop_block` on exactly one ring.
//! Threads that must wait on **two** sources at once — an OS file descriptor
//! *and* a ring, or two rings — can't use that. They poll.
//!
//! A [`Waker`] wraps a counting `eventfd`. Producers call [`Waker::signal`]
//! after publishing to a ring; the waiting thread parks in [`Poller::poll_two`] /
//! [`Waker::wait`] on the eventfd's fd (alongside any other fd) and wakes.
//!
//! Because an eventfd is a *counter*, the classic lost-wakeup race is closed
//! for free: drain your rings, then block. If a producer signals in the gap
//! between your drain and your `poll`, the counter is already non-zero, so
//! `poll` returns immediately instead of sleeping through the event.
//!
//! The fd is shared by cloning ([`Waker`] is `Clone` via an `Arc<OwnedFd>`), so
//! several producer threads can signal the same waiter.

mod poll_set;

pub use poll_set::{Interest, PollSet};

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

/// `poll(2)` readiness flag: data available to read.
pub const POLLIN: i16 = 0x0001;

/// A counting-`eventfd` wakeup channel: many signallers, one waiter.
#[derive(Clone)]
pub struct Waker {
    fd: Arc<OwnedFd>,
}

impl Waker {
    /// Create a fresh non-blocking eventfd wakeup channel.
    ///
    /// # Errors
    /// Returns the OS error if `eventfd(2)` fails.
    pub fn new() -> io::Result<Self> {
        // EFD_NONBLOCK | EFD_CLOEXEC so draining never blocks and the fd
        // doesn't leak across exec.
        const EFD_NONBLOCK: i32 = 0o4000;
        const EFD_CLOEXEC: i32 = 0o2_000_000;
        // SAFETY: plain eventfd2 with an initial count of 0 and standard flags.
        let raw = unsafe { eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: eventfd2 returned a fresh, owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(Self { fd: Arc::new(fd) })
    }

    /// Raw fd to hand to [`Poller`] / `poll(2)`.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Wake the waiting thread: increment the eventfd counter by one. Cheap
    /// (a single 8-byte write); safe to call from any thread.
    pub fn signal(&self) {
        let one: u64 = 1;
        // SAFETY: writing exactly 8 bytes (a u64) to a valid eventfd. A full
        // counter (EAGAIN) is harmless — the waiter will still wake.
        unsafe {
            write(self.fd.as_raw_fd(), std::ptr::addr_of!(one).cast(), 8);
        }
    }

    /// Drain the eventfd counter back to zero. Call after a wake, before
    /// re-checking your rings, so the next [`wait`](Self::wait) actually parks.
    pub fn drain(&self) {
        let mut buf = 0u64;
        // SAFETY: reading up to 8 bytes into a u64; EAGAIN (already drained) is
        // ignored. Loop in case several signals coalesced — one read clears the
        // whole accumulated count, but a fresh signal mid-read just re-arms.
        unsafe { while read(self.fd.as_raw_fd(), std::ptr::addr_of_mut!(buf).cast(), 8) == 8 {} }
    }

    /// Block until this waker is signalled (or `timeout_ms` elapses, if
    /// non-negative). Returns `true` if the eventfd became readable.
    #[must_use]
    pub fn wait(&self, timeout_ms: i32) -> bool {
        Poller::one(self.fd.as_raw_fd()).poll(timeout_ms)
    }
}

/// `struct pollfd`.
#[repr(C)]
pub(crate) struct PollFd {
    pub(crate) fd: RawFd,
    pub(crate) events: i16,
    pub(crate) revents: i16,
}

/// A small `poll(2)` wrapper over one or two raw file descriptors.
///
/// Replaces the free functions `poll_one(fd, timeout)` / `poll_two(a, b,
/// timeout)` with a struct that owns the fd set being waited on, so the
/// operation reads as `Poller::two(a, b).poll(timeout_ms)`.
pub struct Poller {
    fds: [PollFd; 2],
    len: usize,
}

impl Poller {
    /// Wait on a single fd.
    #[must_use]
    pub const fn one(fd: RawFd) -> Self {
        Self {
            fds: [
                PollFd {
                    fd,
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
            ],
            len: 1,
        }
    }

    /// Wait on two fds at once.
    #[must_use]
    pub const fn two(a: RawFd, b: RawFd) -> Self {
        Self {
            fds: [
                PollFd {
                    fd: a,
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: b,
                    events: POLLIN,
                    revents: 0,
                },
            ],
            len: 2,
        }
    }

    /// Block until this poller's single fd is readable, or `timeout_ms`
    /// elapses. Returns whether it became readable.
    ///
    /// # Panics
    /// Panics if called on a [`Self::two`] poller — use
    /// [`poll_two`](Self::poll_two) there.
    #[must_use]
    pub fn poll(&mut self, timeout_ms: i32) -> bool {
        assert!(
            self.len == 1,
            "Poller::poll is for single-fd pollers; use poll_two"
        );
        // SAFETY: pointer + len describe a valid pollfd we own.
        let rc = unsafe { poll(self.fds.as_mut_ptr(), 1, timeout_ms) };
        rc > 0 && self.fds[0].revents & POLLIN != 0
    }

    /// Block until either watched fd is readable.
    ///
    /// `timeout_ms` negative = wait forever. Returns `(a_ready, b_ready)`. A
    /// failed/`EINTR` poll reports `(false, false)` so the caller just
    /// re-checks its sources and loops.
    #[must_use]
    pub fn poll_two(&mut self, timeout_ms: i32) -> (bool, bool) {
        assert!(
            self.len == 2,
            "Poller::poll_two is for two-fd pollers; use poll"
        );
        // SAFETY: pointer + len describe a valid 2-element pollfd array we own.
        let rc = unsafe { poll(self.fds.as_mut_ptr(), 2, timeout_ms) };
        if rc <= 0 {
            return (false, false);
        }
        (
            self.fds[0].revents & POLLIN != 0,
            self.fds[1].revents & POLLIN != 0,
        )
    }
}

unsafe extern "C" {
    fn eventfd(initval: u32, flags: i32) -> RawFd;
    pub(crate) fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
    fn read(fd: RawFd, buf: *mut core::ffi::c_void, count: usize) -> isize;
    fn write(fd: RawFd, buf: *const core::ffi::c_void, count: usize) -> isize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_then_wait_returns_immediately() {
        let w = Waker::new().unwrap();
        w.signal();
        assert!(w.wait(1000));
    }

    #[test]
    fn drain_resets_readiness() {
        let w = Waker::new().unwrap();
        w.signal();
        w.drain();
        // After draining, a short wait should time out (no pending signal).
        assert!(!w.wait(20));
    }

    #[test]
    fn signal_wakes_blocked_thread() {
        let w = Waker::new().unwrap();
        let w2 = w.clone();
        let h = std::thread::spawn(move || w2.wait(2000));
        std::thread::sleep(std::time::Duration::from_millis(50));
        w.signal();
        assert!(h.join().unwrap());
    }

    #[test]
    fn poll_two_reports_correct_side() {
        let a = Waker::new().unwrap();
        let b = Waker::new().unwrap();
        b.signal();
        let (ra, rb) = Poller::two(a.raw_fd(), b.raw_fd()).poll_two(1000);
        assert!(!ra);
        assert!(rb);
    }
}
