//! [`PollSet`] -- `poll(2)` over any number of fds, each with its own interest.
//!
//! [`Poller`](crate::Poller) covers one or two fds and only ever asks for
//! `POLLIN`. A watcher of kernel tables needs more of both: `/proc/self/mountinfo`
//! signals a change with `POLLPRI` (reported alongside `POLLERR`), never `POLLIN`.

use std::os::fd::RawFd;

use crate::{POLLIN, PollFd, poll};

const POLLPRI: i16 = 0x0002;

/// What a watched fd is waited on for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interest {
    Readable,
    Priority,
}

impl Interest {
    const fn events(self) -> i16 {
        match self {
            Self::Readable => POLLIN,
            Self::Priority => POLLPRI,
        }
    }
}

/// A fixed set of fds waited on together.
pub struct PollSet {
    fds: Vec<PollFd>,
}

impl PollSet {
    #[must_use]
    pub const fn new() -> Self {
        Self { fds: Vec::new() }
    }

    /// Add `fd` and return the index [`Self::is_ready`] answers for.
    pub fn watch(&mut self, fd: RawFd, interest: Interest) -> usize {
        self.fds.push(PollFd {
            fd,
            events: interest.events(),
            revents: 0,
        });
        self.fds.len() - 1
    }

    /// Block until any watched fd is ready, or `timeout_ms` elapses (negative
    /// waits forever). Returns whether any fd is ready; an interrupted or
    /// failed wait reports `false`.
    #[must_use]
    pub fn wait(&mut self, timeout_ms: i32) -> bool {
        for entry in &mut self.fds {
            entry.revents = 0;
        }
        // SAFETY: pointer and length describe the live pollfd vector we own.
        let ready = unsafe { poll(self.fds.as_mut_ptr(), self.fds.len() as u64, timeout_ms) };
        ready > 0
    }

    /// Whether the fd at `index` was ready in the last [`Self::wait`].
    #[must_use]
    pub fn is_ready(&self, index: usize) -> bool {
        self.fds.get(index).is_some_and(|entry| entry.revents != 0)
    }
}

impl Default for PollSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Interest, PollSet};
    use crate::Waker;

    #[test]
    fn an_idle_set_times_out() {
        let quiet = Waker::new().unwrap();
        let mut set = PollSet::new();
        set.watch(quiet.raw_fd(), Interest::Readable);
        assert!(!set.wait(0));
    }

    #[test]
    fn readiness_is_reported_for_the_right_index() {
        let quiet = Waker::new().unwrap();
        let loud = Waker::new().unwrap();
        loud.signal();
        let mut set = PollSet::new();
        let first = set.watch(quiet.raw_fd(), Interest::Readable);
        let second = set.watch(loud.raw_fd(), Interest::Readable);
        let third = set.watch(quiet.raw_fd(), Interest::Readable);

        assert!(set.wait(1000));
        assert!(!set.is_ready(first));
        assert!(set.is_ready(second));
        assert!(!set.is_ready(third));
    }

    #[test]
    fn a_priority_interest_ignores_plain_readability() {
        let loud = Waker::new().unwrap();
        loud.signal();
        let mut set = PollSet::new();
        set.watch(loud.raw_fd(), Interest::Priority);
        assert!(!set.wait(0));
    }

    #[test]
    fn readiness_clears_once_drained() {
        let waker = Waker::new().unwrap();
        waker.signal();
        let mut set = PollSet::new();
        let index = set.watch(waker.raw_fd(), Interest::Readable);
        assert!(set.wait(0));
        waker.drain();
        assert!(!set.wait(0));
        assert!(!set.is_ready(index));
    }

    #[test]
    fn an_unknown_index_is_not_ready() {
        assert!(!PollSet::new().is_ready(3));
    }
}
