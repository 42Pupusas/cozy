use quetzalcoatl::mpmc;
use quetzalcoatl::mpsc;

use super::scan_job::{ScanJob, Scanned, Work};
use crate::walk::ScanScratch;

/// A pool thread: takes [`ScanJob`]s until the coordinator closes the job
/// channel, and sends each result back.
pub struct ScanWorker {
    jobs: mpmc::Consumer<ScanJob>,
    results: mpsc::Producer<Scanned>,
    scratch: ScanScratch,
}

impl ScanWorker {
    #[must_use]
    pub fn new(jobs: mpmc::Consumer<ScanJob>, results: mpsc::Producer<Scanned>) -> Self {
        Self {
            jobs,
            results,
            scratch: ScanScratch::new(),
        }
    }

    pub fn run(mut self) {
        while let Some(job) = self.jobs.pop_block() {
            let scanned = self.perform(job);
            if self.results.push_block(scanned).is_err() {
                return;
            }
        }
    }

    fn perform(&mut self, job: ScanJob) -> Scanned {
        let ScanJob {
            root,
            scan,
            work,
            mut contents,
        } = job;
        match work {
            Work::Dir(dir) => scan.scan(&dir, &mut self.scratch, &mut contents),
            Work::Chunk(chunk) => scan.stat_chunk(chunk, &mut self.scratch, &mut contents),
        };
        Scanned { root, contents }
    }
}
