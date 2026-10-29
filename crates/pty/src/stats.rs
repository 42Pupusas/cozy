//! Process-wide counts of failed master-fd reads and writes, by errno.
//!
//! The event loop retries `EINTR` and `EAGAIN` without losing bytes, so
//! these failures are invisible in the output. The counts let a profiler
//! tell what the failed completions it sees actually were.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::inflight::OpTag;
use crate::{EAGAIN, EINTR, ENOBUFS};

static READS: Counters = Counters::new();
static WRITES: Counters = Counters::new();

/// Owner of the counters; every [`Pty`](crate::Pty) records into the same
/// totals.
pub struct Stats;

/// The counters' values at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub read: ErrnoCounts,
    pub write: ErrnoCounts,
}

/// Failed completions of one operation kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ErrnoCounts {
    pub eintr: u64,
    pub eagain: u64,
    /// The read pool ran dry and the multishot read stopped.
    pub enobufs: u64,
    /// Any other errno, including the `EIO` that ends a session.
    pub other: u64,
}

struct Counters {
    eintr: AtomicU64,
    eagain: AtomicU64,
    enobufs: AtomicU64,
    other: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            eintr: AtomicU64::new(0),
            eagain: AtomicU64::new(0),
            enobufs: AtomicU64::new(0),
            other: AtomicU64::new(0),
        }
    }

    fn record(&self, result: i32) {
        let counter = match result {
            0.. => return,
            r if r == -EINTR => &self.eintr,
            r if r == -EAGAIN => &self.eagain,
            r if r == -ENOBUFS => &self.enobufs,
            _ => &self.other,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> ErrnoCounts {
        ErrnoCounts {
            eintr: self.eintr.load(Ordering::Relaxed),
            eagain: self.eagain.load(Ordering::Relaxed),
            enobufs: self.enobufs.load(Ordering::Relaxed),
            other: self.other.load(Ordering::Relaxed),
        }
    }
}

impl Stats {
    pub(crate) fn record(tag: OpTag, result: i32) {
        match tag {
            OpTag::Read => READS.record(result),
            OpTag::Write => WRITES.record(result),
            _ => {}
        }
    }

    #[must_use]
    pub fn snapshot() -> StatsSnapshot {
        StatsSnapshot {
            read: READS.snapshot(),
            write: WRITES.snapshot(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Counters, ErrnoCounts};
    use crate::{EAGAIN, EINTR, ENOBUFS};

    #[test]
    fn each_errno_lands_in_its_own_counter() {
        let counters = Counters::new();
        counters.record(-EINTR);
        counters.record(-EINTR);
        counters.record(-EAGAIN);
        counters.record(-ENOBUFS);
        counters.record(-5);
        let expected = ErrnoCounts {
            eintr: 2,
            eagain: 1,
            enobufs: 1,
            other: 1,
        };
        assert_eq!(counters.snapshot(), expected);
    }

    #[test]
    fn successful_results_are_not_counted() {
        let counters = Counters::new();
        counters.record(0);
        counters.record(4096);
        assert_eq!(counters.snapshot(), ErrnoCounts::default());
    }
}
