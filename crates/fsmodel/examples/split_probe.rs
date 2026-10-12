//! Checks how directory scanning scales across threads: the root's
//! subdirectories are dealt round-robin to N threads, each walking its share
//! serially with its own scratch. No shared state; totals come back on join.
//!
//! `split_probe <dir> [rounds] [max_threads] [seeds_per_thread]`

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fsmodel::walk::{DirContents, DirScan, ScanScratch};

struct Share {
    scan: DirScan,
    dirs: Vec<PathBuf>,
}

impl Share {
    fn walk(mut self) -> (u64, Duration, u64) {
        let start = Instant::now();
        let mut scanned = 0;
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        let mut bytes = 0;
        while let Some(dir) = self.dirs.pop() {
            scanned += 1;
            bytes += self.scan.scan(&dir, &mut scratch, &mut contents).bytes();
            self.dirs.extend(contents.drain_subdirs());
        }
        (bytes, start.elapsed(), scanned)
    }
}

struct SplitProbe {
    root: PathBuf,
    scan: DirScan,
    rounds: u32,
    max_threads: usize,
    fanout: usize,
}

impl SplitProbe {
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        let root = PathBuf::from(
            args.next()
                .expect("usage: split_probe <dir> [rounds] [max_threads]"),
        );
        let rounds = args.next().map_or(10, |raw| raw.parse().expect("rounds"));
        let max_threads = args
            .next()
            .map_or(16, |raw| raw.parse().expect("max_threads"));
        let fanout = args.next().map_or(1, |raw| raw.parse().expect("fanout"));
        let device = std::fs::symlink_metadata(&root).expect("stat root").dev();
        Self {
            root,
            scan: DirScan::new(device),
            rounds,
            max_threads,
            fanout,
        }
    }

    fn seeds(&self, wanted: usize) -> (u64, Vec<PathBuf>) {
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        let mut bytes = 0;
        let mut seeds = std::collections::VecDeque::from([self.root.clone()]);
        while seeds.len() < wanted {
            let Some(dir) = seeds.pop_front() else {
                break;
            };
            bytes += self.scan.scan(&dir, &mut scratch, &mut contents).bytes();
            seeds.extend(contents.drain_subdirs());
        }
        (bytes, seeds.into())
    }

    fn round(&self, threads: usize) -> (u64, Vec<(Duration, u64)>) {
        let (root_bytes, seeds) = self.seeds(threads * self.fanout);
        let mut shares: Vec<Vec<PathBuf>> = (0..threads).map(|_| Vec::new()).collect();
        for (index, seed) in seeds.into_iter().enumerate() {
            shares[index % threads].push(seed);
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = shares
                .into_iter()
                .map(|dirs| {
                    let share = Share {
                        scan: self.scan,
                        dirs,
                    };
                    scope.spawn(move || share.walk())
                })
                .collect();
            let mut bytes = root_bytes;
            let mut busy = Vec::with_capacity(threads);
            for handle in handles {
                let (share_bytes, spent, scanned) = handle.join().expect("walker");
                bytes += share_bytes;
                busy.push((spent, scanned));
            }
            (bytes, busy)
        })
    }

    fn run(&self) {
        let mut threads = 1;
        let mut baseline = Duration::ZERO;
        while threads <= self.max_threads {
            let mut best = Duration::MAX;
            let mut bytes = 0;
            let mut busy = Vec::new();
            for _ in 0..self.rounds {
                let start = Instant::now();
                let (round_bytes, round_busy) = self.round(threads);
                let spent = start.elapsed();
                if spent < best {
                    best = spent;
                    busy = round_busy;
                }
                bytes = round_bytes;
            }
            let summed: Duration = busy.iter().map(|(spent, _)| *spent).sum();
            let slowest = busy.iter().max().copied().unwrap_or_default();
            if threads == 1 {
                baseline = best;
            }
            println!(
                "{threads:>2} threads  best {best:>12?}  speedup {:>5.2}x  cpu-sum {summed:>12?}  slowest {:>12?} ({} dirs)  ({bytes} bytes)",
                baseline.as_secs_f64() / best.as_secs_f64(),
                slowest.0,
                slowest.1,
            );
            threads *= 2;
        }
    }
}

fn main() {
    SplitProbe::from_args().run();
}
