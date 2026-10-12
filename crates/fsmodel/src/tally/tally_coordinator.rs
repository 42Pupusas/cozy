use quetzalcoatl::mpmc;
use quetzalcoatl::mpsc;

use super::keepalive::KeepaliveWatch;
use super::scan_job::{ScanJob, Scanned};
use super::tallied::Tallied;
use super::walk_book::WalkBook;
use crate::walk::DirContents;

/// Owns every root's ledger, feeds the worker pool, and reports totals.
///
/// At most `in_flight_limit` jobs are out at once, and both channels hold at
/// least that many, so neither the coordinator nor a worker ever blocks on
/// a full channel while the other waits on it.
pub struct TallyCoordinator<N> {
    book: WalkBook,
    jobs: mpmc::Producer<ScanJob>,
    results: mpsc::Consumer<Scanned>,
    reports: mpsc::Producer<Tallied>,
    keepalive: KeepaliveWatch,
    notify: N,
    in_flight_limit: usize,
    spare: Vec<DirContents>,
}

impl<N: Fn()> TallyCoordinator<N> {
    #[must_use]
    pub const fn new(
        book: WalkBook,
        jobs: mpmc::Producer<ScanJob>,
        results: mpsc::Consumer<Scanned>,
        reports: mpsc::Producer<Tallied>,
        keepalive: KeepaliveWatch,
        notify: N,
        in_flight_limit: usize,
    ) -> Self {
        Self {
            book,
            jobs,
            results,
            reports,
            keepalive,
            notify,
            in_flight_limit,
            spare: Vec::new(),
        }
    }

    pub fn run(mut self) {
        let opened: Vec<Tallied> = self.book.settled_at_open().collect();
        for tallied in opened {
            if !self.publish(tallied) {
                return;
            }
        }
        let mut in_flight = 0_usize;
        loop {
            while in_flight < self.in_flight_limit {
                let Some((root, scan, work)) = self.book.next() else {
                    break;
                };
                let contents = self.spare.pop().unwrap_or_default();
                let job = ScanJob {
                    root,
                    scan,
                    work,
                    contents,
                };
                if self.jobs.push_block(job).is_err() {
                    return;
                }
                in_flight += 1;
            }
            if in_flight == 0 || self.keepalive.is_abandoned() {
                return;
            }
            let Some(Scanned { root, mut contents }) = self.results.pop_block() else {
                return;
            };
            in_flight -= 1;
            if let Some(tallied) = self.book.absorb(root, &mut contents)
                && !self.publish(tallied)
            {
                return;
            }
            contents.clear();
            self.spare.push(contents);
        }
    }

    fn publish(&self, tallied: Tallied) -> bool {
        if self.reports.push_block(tallied).is_err() {
            return false;
        }
        (self.notify)();
        true
    }
}
