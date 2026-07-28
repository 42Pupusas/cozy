//! Reporting how far a long operation has got, and letting it be stopped.
//!
//! Copying 40 GB must not freeze the UI and must be abortable. Those are two
//! halves of the same contract -- the engine periodically publishes where it
//! is *and* checks whether it should stop -- so they live together here,
//! with no knowledge of what is being copied.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A snapshot of an operation's progress.
///
/// Plain `Copy` data rather than a borrow of live counters: the UI thread
/// renders this while the worker thread keeps going, and a torn read of
/// several independently-updating atomics would show impossible states
/// (bytes beyond the total, files counted twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressSnapshot {
    /// Bytes transferred so far.
    pub bytes_done: u64,
    /// Bytes expected in total, if a survey has run. Zero means unknown --
    /// the UI should then show an indeterminate spinner, not "0%".
    pub bytes_total: u64,
    /// Entries (files, directories, symlinks) finished so far.
    pub files_done: u64,
    /// Entries expected in total, if known.
    pub files_total: u64,
}

impl ProgressSnapshot {
    /// Completion as a fraction in `0.0..=1.0`, or `None` when the total
    /// isn't known yet.
    ///
    /// `None` rather than `0.0` so the caller can't accidentally render a
    /// confident-looking "0%" for an operation that simply hasn't been
    /// surveyed.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        if self.bytes_total == 0 {
            return None;
        }
        // Clamped because the survey and the transfer race: a file that grew
        // after being surveyed would otherwise push this above 1.0 and make
        // a progress bar overflow its track.
        #[allow(
            clippy::cast_precision_loss,
            reason = "f64 has 52 bits of mantissa; exact up to 4 PiB, and this only drives a progress bar"
        )]
        let f = self.bytes_done as f64 / self.bytes_total as f64;
        Some(f.clamp(0.0, 1.0))
    }

    /// Whether a survey has established totals.
    #[must_use]
    pub const fn is_determinate(&self) -> bool {
        self.bytes_total > 0 || self.files_total > 0
    }
}

/// Shared, thread-safe progress counters plus a cancellation flag.
///
/// Cloneable and cheap: the worker holds one, the UI holds another, and both
/// refer to the same counters. Atomics rather than a `Mutex` because the
/// worker updates these on every buffer it writes -- taking a lock on that
/// path would make the reporting cost more than the copying.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    inner: Arc<ProgressInner>,
}

#[derive(Debug, Default)]
struct ProgressInner {
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    files_done: AtomicU64,
    files_total: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    /// A fresh, un-cancelled progress handle with unknown totals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the totals discovered by a survey pass.
    pub fn set_totals(&self, bytes: u64, files: u64) {
        self.inner.bytes_total.store(bytes, Ordering::Relaxed);
        self.inner.files_total.store(files, Ordering::Relaxed);
    }

    /// Record `n` more bytes transferred.
    pub fn add_bytes(&self, n: u64) {
        self.inner.bytes_done.fetch_add(n, Ordering::Relaxed);
    }

    /// Record one more finished entry.
    pub fn add_file(&self) {
        self.inner.files_done.fetch_add(1, Ordering::Relaxed);
    }

    /// Ask the operation to stop at its next checkpoint.
    ///
    /// Cooperative, not immediate: the engine finishes the buffer it is
    /// writing so it never abandons a file mid-`write` and leaves a
    /// half-written block behind without knowing it.
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Relaxed)
    }

    /// A consistent-enough snapshot for display.
    #[must_use]
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            bytes_done: self.inner.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.inner.bytes_total.load(Ordering::Relaxed),
            files_done: self.inner.files_done.load(Ordering::Relaxed),
            files_total: self.inner.files_total.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_progress_is_indeterminate() {
        let p = Progress::new();
        let s = p.snapshot();
        assert_eq!(s.fraction(), None);
        assert!(!s.is_determinate());
        assert!(!p.is_cancelled());
    }

    #[test]
    fn totals_make_it_determinate() {
        let p = Progress::new();
        p.set_totals(100, 4);
        assert!(p.snapshot().is_determinate());
    }

    #[test]
    fn bytes_accumulate_into_a_fraction() {
        let p = Progress::new();
        p.set_totals(200, 2);
        p.add_bytes(50);
        assert_eq!(p.snapshot().fraction(), Some(0.25));
        p.add_bytes(150);
        assert_eq!(p.snapshot().fraction(), Some(1.0));
    }

    #[test]
    fn a_file_that_grew_mid_copy_cannot_exceed_full() {
        // The survey and the copy race; a progress bar must never overflow.
        let p = Progress::new();
        p.set_totals(100, 1);
        p.add_bytes(500);
        assert_eq!(p.snapshot().fraction(), Some(1.0));
    }

    #[test]
    fn files_are_counted_separately_from_bytes() {
        // An 800-file tree of empty files has zero bytes but real progress.
        let p = Progress::new();
        p.set_totals(0, 3);
        p.add_file();
        p.add_file();
        let s = p.snapshot();
        assert_eq!(s.files_done, 2);
        assert_eq!(s.files_total, 3);
        assert_eq!(s.bytes_done, 0);
    }

    #[test]
    fn cancellation_is_visible_through_a_clone() {
        // The whole point: the UI holds one handle, the worker another.
        let ui = Progress::new();
        let worker = ui.clone();
        assert!(!worker.is_cancelled());
        ui.cancel();
        assert!(worker.is_cancelled());
    }

    #[test]
    fn counters_are_shared_across_clones() {
        let a = Progress::new();
        let b = a.clone();
        b.add_bytes(7);
        b.add_file();
        assert_eq!(a.snapshot().bytes_done, 7);
        assert_eq!(a.snapshot().files_done, 1);
    }

    #[test]
    fn updates_from_many_threads_are_not_lost() {
        // Guards the choice of atomics: `+=` on a plain integer would drop
        // updates here, and the progress bar would stall short of the end.
        let p = Progress::new();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = p.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    p.add_bytes(1);
                    p.add_file();
                }
            }));
        }
        for h in handles {
            h.join().expect("thread panicked");
        }
        let s = p.snapshot();
        assert_eq!(s.bytes_done, 8000);
        assert_eq!(s.files_done, 8000);
    }
}
