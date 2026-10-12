//! [`SizeTally`] -- recursive totals for a batch of directories, measured by
//! a pool of scan threads so a large tree never stalls the UI.
//!
//! A coordinator thread owns every root's ledger and hands one directory, or
//! one chunk of a large directory's names, to the pool at a time. Totals are
//! reported as they grow, and once more when final.
//!
//! Dropping the tally cancels the walk, so navigating away abandons a scan
//! of the directory being left instead of finishing it for nobody.

mod keepalive;
mod report_gate;
mod root_walk;
mod scan_job;
mod scan_worker;
mod tallied;
mod tally_coordinator;
mod walk_book;

pub use tallied::Tallied;

use std::path::PathBuf;

use keepalive::Keepalive;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpmc;
use quetzalcoatl::mpsc::{self, Consumer};
use scan_worker::ScanWorker;
use tally_coordinator::TallyCoordinator;
use walk_book::WalkBook;

pub struct SizeTally {
    reports: Consumer<Tallied>,
    _keepalive: Keepalive,
}

impl SizeTally {
    const REPORT_SLOTS: usize = 64;
    const MAX_WORKERS: usize = 8;
    const JOBS_PER_WORKER: usize = 2;
    const CHUNK_NAMES: usize = 256;

    /// Total every path in `dirs`, calling `notify` after each report is
    /// queued so the owner can wake whatever loop will call [`Self::drain`].
    pub fn start<N>(dirs: Vec<PathBuf>, notify: N) -> std::io::Result<Self>
    where
        N: Fn() + Send + 'static,
    {
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(Self::MAX_WORKERS);
        Self::start_with(dirs, notify, workers, Self::CHUNK_NAMES)
    }

    /// As [`start`](Self::start), with `workers` scan threads and large
    /// directories split into chunks of `chunk_names`.
    ///
    /// # Panics
    ///
    /// If `workers` or `chunk_names` is zero.
    pub fn start_with<N>(
        dirs: Vec<PathBuf>,
        notify: N,
        workers: usize,
        chunk_names: usize,
    ) -> std::io::Result<Self>
    where
        N: Fn() + Send + 'static,
    {
        assert!(workers > 0, "a tally needs at least one scan thread");
        let in_flight = workers * Self::JOBS_PER_WORKER;
        let channel = Capacity::at_least(in_flight.max(4));
        let (jobs, job_queue) = mpmc::RingBuffer::new(channel).split();
        let (results, result_queue) = mpsc::RingBuffer::new(channel).split();
        let (reports, report_queue) =
            mpsc::RingBuffer::new(Capacity::exact(Self::REPORT_SLOTS)).split();
        let (keepalive, watch) = Keepalive::pair();

        for index in 0..workers {
            let worker = ScanWorker::new(job_queue.clone(), results.clone());
            std::thread::Builder::new()
                .name(format!("size-scan-{index}"))
                .spawn(move || worker.run())?;
        }
        drop(job_queue);
        drop(results);

        let coordinator = TallyCoordinator::new(
            WalkBook::open(dirs, chunk_names),
            jobs,
            result_queue,
            reports,
            watch,
            notify,
            in_flight,
        );
        std::thread::Builder::new()
            .name("size-tally".to_owned())
            .spawn(move || coordinator.run())?;
        Ok(Self {
            reports: report_queue,
            _keepalive: keepalive,
        })
    }

    /// Every report that has arrived since the last drain. Never blocks.
    #[must_use]
    pub fn drain(&mut self) -> Vec<Tallied> {
        let mut arrived = Vec::new();
        self.reports.drain(|tallied| arrived.push(tallied));
        arrived
    }
}

#[cfg(test)]
mod tests {
    use super::{SizeTally, Tallied};
    use crate::test_support::TempDir;
    use crate::walk::DirWalk;
    use quetzalcoatl::capacity::Capacity;
    use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn wake_ring() -> (Producer<()>, Consumer<()>) {
        RingBuffer::new(Capacity::exact(64)).split()
    }

    fn start(dirs: Vec<PathBuf>) -> (SizeTally, Consumer<()>) {
        let (wake, wakes) = wake_ring();
        let tally = SizeTally::start(dirs, move || {
            let _ = wake.push_block(());
        })
        .expect("spawn");
        (tally, wakes)
    }

    fn settle(tally: &mut SizeTally, wakes: &mut Consumer<()>, want: usize) -> Vec<Tallied> {
        let mut got = Vec::new();
        while got
            .iter()
            .filter(|tallied: &&Tallied| tallied.settled)
            .count()
            < want
        {
            wakes
                .pop_block()
                .expect("the tally signals each report before it exits");
            got.extend(tally.drain());
        }
        got
    }

    fn finals(reports: &[Tallied]) -> HashMap<PathBuf, u64> {
        reports
            .iter()
            .filter(|tallied| tallied.settled)
            .map(|tallied| (tallied.path.clone(), tallied.bytes))
            .collect()
    }

    fn reference(path: &std::path::Path) -> u64 {
        let cancel = AtomicBool::new(false);
        DirWalk::new(&cancel).total(path).expect("not cancelled")
    }

    #[test]
    fn each_directory_settles_at_its_total() {
        let dir = TempDir::new("tally_each");
        let a = dir.create_dir("a");
        let b = dir.create_dir("b");
        dir.create_file("a/x", &[0; 3]);
        dir.create_file("b/y/z", &[0; 40]);
        let (mut tally, mut wakes) = start(vec![a.clone(), b.clone()]);
        let got = finals(&settle(&mut tally, &mut wakes, 2));
        assert_eq!(got, HashMap::from([(a, 3), (b, 40)]));
    }

    #[test]
    fn totals_match_the_serial_walk_on_a_tree_with_large_directories_and_links() {
        let dir = TempDir::new("tally_reference");
        for i in 0..700 {
            dir.create_file(&format!("big/f{i}"), &vec![0; i % 13]);
        }
        for i in 0..40 {
            dir.create_file(&format!("deep/{i}/inner/g"), &vec![0; i]);
        }
        let shared = dir.create_file("deep/shared", &[0; 77]);
        std::fs::hard_link(&shared, dir.path().join("big/shared-again")).expect("link");
        dir.create_symlink("deep/link", "big");
        let roots = vec![
            dir.path().join("big"),
            dir.path().join("deep"),
            dir.path().to_path_buf(),
        ];
        let (mut tally, mut wakes) = start(roots.clone());
        let got = finals(&settle(&mut tally, &mut wakes, roots.len()));
        for root in roots {
            assert_eq!(got[&root], reference(&root), "{}", root.display());
        }
    }

    #[test]
    fn partial_totals_only_grow_toward_the_final_total() {
        let dir = TempDir::new("tally_partial");
        for i in 0..50 {
            dir.create_file(&format!("t/{i}/f"), &[0; 100]);
        }
        let root = dir.path().join("t");
        let (mut tally, mut wakes) = start(vec![root]);
        let reports = settle(&mut tally, &mut wakes, 1);
        let last = reports.last().expect("a report");
        assert!(last.settled, "the final total is the last report");
        assert_eq!(last.bytes, 5000);
        let partials: Vec<u64> = reports.iter().map(|tallied| tallied.bytes).collect();
        assert!(partials.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            reports[..reports.len() - 1]
                .iter()
                .all(|tallied| !tallied.settled)
        );
    }

    #[test]
    fn files_and_missing_paths_settle_without_scanning() {
        let dir = TempDir::new("tally_odd_roots");
        let file = dir.create_file("f", &[0; 6]);
        let gone = dir.path().join("gone");
        let (mut tally, mut wakes) = start(vec![file.clone(), gone.clone()]);
        let got = finals(&settle(&mut tally, &mut wakes, 2));
        assert_eq!(got, HashMap::from([(file, 6), (gone, 0)]));
    }

    #[test]
    fn an_empty_batch_finishes_without_signalling() {
        let (mut tally, mut wakes) = start(Vec::new());
        assert_eq!(
            wakes.pop_block(),
            None,
            "the tally exits without signalling"
        );
        assert!(tally.drain().is_empty());
    }

    #[test]
    fn dropping_the_tally_stops_the_walk() {
        let dir = TempDir::new("tally_drop");
        for i in 0..200 {
            dir.create_file(&format!("d/{i}"), b"x");
        }
        let batch = 5_000;
        let (tally, mut wakes) = start(vec![dir.path().join("d"); batch]);
        drop(tally);
        let mut reported = 0;
        while wakes.pop_block().is_some() {
            reported += 1;
        }
        assert!(reported < batch, "all {reported} roots were reported");
    }
}
