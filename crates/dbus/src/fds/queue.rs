//! Staging for descriptors that arrived ahead of the message owning them.

use std::collections::VecDeque;
use std::os::fd::OwnedFd;

/// Descriptors received off the bus socket but not yet claimed by a parsed
/// message.
///
/// A stream socket does not deliver ancillary data on message boundaries.
/// The kernel attaches each `SCM_RIGHTS` item to whichever byte chunk it
/// rode in with, and `recv_exact` may need several receives to complete one
/// D-Bus frame. So descriptors are staged here in arrival order, and a
/// finished frame claims the first `n` of them, where `n` is its `UNIX_FDS`
/// header field.
///
/// Ordering is the whole contract: D-Bus `h` values are indexes into the
/// message's descriptor array, so claiming out of order silently hands a
/// caller the wrong file.
#[derive(Debug, Default)]
pub struct FdQueue {
    staged: VecDeque<OwnedFd>,
}

impl FdQueue {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            staged: VecDeque::new(),
        }
    }

    /// Stage descriptors that arrived with one receive, preserving order.
    pub fn push_received(&mut self, fds: Vec<OwnedFd>) {
        self.staged.extend(fds);
    }

    /// How many descriptors are staged but unclaimed.
    #[must_use]
    pub fn staged_len(&self) -> usize {
        self.staged.len()
    }

    /// Claim the first `count` descriptors for a completed message.
    ///
    /// Returns `None` when fewer than `count` are staged, leaving the queue
    /// untouched: a message whose descriptors have not all arrived must not
    /// receive a short array it would index out of bounds.
    #[must_use]
    pub fn claim(&mut self, count: usize) -> Option<Vec<OwnedFd>> {
        if count > self.staged.len() {
            return None;
        }
        Some(self.staged.drain(..count).collect())
    }

    /// Close every staged descriptor.
    ///
    /// Called when a connection fails mid-message: the frame those
    /// descriptors belonged to will never be parsed, so nothing can ever
    /// claim them and holding them would leak until the process exits.
    pub fn clear(&mut self) {
        self.staged.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use super::*;
    use crate::fds::test_fds::{FdTestGuard, LoanedFds};

    #[test]
    fn a_fresh_queue_is_empty() {
        let queue = FdQueue::new();
        assert_eq!(queue.staged_len(), 0);
    }

    #[test]
    fn claiming_from_an_empty_queue_yields_none() {
        let mut queue = FdQueue::new();
        assert!(queue.claim(1).is_none());
        assert!(queue.claim(0).is_some(), "claiming nothing always succeeds");
    }

    #[test]
    fn staged_descriptors_are_claimed_in_arrival_order() {
        let mut queue = FdQueue::new();
        let first = LoanedFds::open(2);
        let second = LoanedFds::open(1);
        let expected: Vec<i32> = first
            .raw_numbers()
            .into_iter()
            .chain(second.raw_numbers())
            .collect();

        queue.push_received(first.into_owned());
        queue.push_received(second.into_owned());
        assert_eq!(queue.staged_len(), 3);

        let claimed = queue.claim(3).expect("three staged");
        let got: Vec<i32> = claimed.iter().map(AsRawFd::as_raw_fd).collect();
        assert_eq!(got, expected, "arrival order defines the h index");
    }

    /// The reason the queue exists: descriptors can arrive before the frame
    /// that owns them is fully read, and a later frame's descriptors can
    /// already be staged behind them.
    #[test]
    fn a_partial_claim_leaves_the_rest_staged_in_order() {
        let mut queue = FdQueue::new();
        let loaned = LoanedFds::open(4);
        let expected = loaned.raw_numbers();
        queue.push_received(loaned.into_owned());

        let first = queue.claim(1).expect("one staged");
        assert_eq!(first[0].as_raw_fd(), expected[0]);
        assert_eq!(queue.staged_len(), 3);

        let rest = queue.claim(3).expect("three remain");
        let got: Vec<i32> = rest.iter().map(AsRawFd::as_raw_fd).collect();
        assert_eq!(got, expected[1..]);
        assert_eq!(queue.staged_len(), 0);
    }

    /// A short claim must not half-drain the queue, or the descriptors it
    /// took would be lost while the message that wanted them fails anyway.
    #[test]
    fn a_claim_larger_than_the_queue_takes_nothing() {
        let mut queue = FdQueue::new();
        queue.push_received(LoanedFds::open(2).into_owned());
        assert!(queue.claim(3).is_none());
        assert_eq!(queue.staged_len(), 2, "the queue is left intact");
        assert!(queue.claim(2).is_some(), "still claimable afterwards");
    }

    #[test]
    fn clearing_closes_every_staged_descriptor() {
        let _guard = FdTestGuard::acquire();
        let mut queue = FdQueue::new();
        let loaned = LoanedFds::open(3);
        let raw = loaned.raw_numbers();
        queue.push_received(loaned.into_owned());
        queue.clear();
        assert_eq!(queue.staged_len(), 0);
        for fd in raw {
            assert!(!LoanedFds::is_open(fd), "fd {fd} must be closed by clear()");
        }
    }

    #[test]
    fn dropping_the_queue_closes_what_is_still_staged() {
        let _guard = FdTestGuard::acquire();
        let loaned = LoanedFds::open(2);
        let raw = loaned.raw_numbers();
        {
            let mut queue = FdQueue::new();
            queue.push_received(loaned.into_owned());
        }
        for fd in raw {
            assert!(!LoanedFds::is_open(fd), "fd {fd} must close with the queue");
        }
    }
}
