//! Times [`SizeTally`] on a real tree: one root, or each child of a root
//! the way the file browser asks for them. Prints time to the first report,
//! time to the last final total, and the total itself.
//!
//! `tally_probe <dir> [rounds] [root|children] [workers] [chunk_names]`

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fsmodel::tally::SizeTally;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::RingBuffer;

struct RoundTiming {
    first: Duration,
    done: Duration,
    bytes: u64,
    reports: usize,
}

struct TallyProbe {
    roots: Vec<PathBuf>,
    rounds: u32,
    workers: Option<usize>,
    chunk_names: usize,
}

impl TallyProbe {
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        let root = PathBuf::from(
            args.next()
                .expect("usage: tally_probe <dir> [rounds] [root|children]"),
        );
        let rounds = args.next().map_or(10, |raw| raw.parse().expect("rounds"));
        let roots = match args.next().as_deref() {
            None | Some("root") => vec![root],
            Some("children") => std::fs::read_dir(&root)
                .expect("read root")
                .flatten()
                .map(|item| item.path())
                .collect(),
            Some(other) => panic!("unknown mode {other}"),
        };
        let workers = args.next().map(|raw| raw.parse().expect("workers"));
        let chunk_names = args
            .next()
            .map_or(256, |raw| raw.parse().expect("chunk_names"));
        Self {
            roots,
            rounds,
            workers,
            chunk_names,
        }
    }

    fn round(&self) -> RoundTiming {
        let (wake, mut wakes) = RingBuffer::new(Capacity::exact(64)).split();
        let start = Instant::now();
        let notify = move || {
            let _ = wake.push_block(());
        };
        let mut tally = match self.workers {
            None => SizeTally::start(self.roots.clone(), notify),
            Some(workers) => {
                SizeTally::start_with(self.roots.clone(), notify, workers, self.chunk_names)
            }
        }
        .expect("spawn");
        let mut timing = RoundTiming {
            first: Duration::ZERO,
            done: Duration::ZERO,
            bytes: 0,
            reports: 0,
        };
        let mut settled = 0;
        while settled < self.roots.len() {
            if wakes.pop_block().is_none() {
                break;
            }
            for tallied in tally.drain() {
                if timing.reports == 0 {
                    timing.first = start.elapsed();
                }
                timing.reports += 1;
                if tallied.settled {
                    settled += 1;
                    timing.bytes += tallied.bytes;
                }
            }
        }
        timing.done = start.elapsed();
        timing
    }

    fn run(&self) {
        let mut best: Option<RoundTiming> = None;
        for _ in 0..self.rounds {
            let timing = self.round();
            if best.as_ref().is_none_or(|kept| timing.done < kept.done) {
                best = Some(timing);
            }
        }
        let best = best.expect("at least one round");
        println!(
            "{} roots  best done {:?}  first report {:?}  {} reports  {} bytes",
            self.roots.len(),
            best.done,
            best.first,
            best.reports,
            best.bytes
        );
    }
}

fn main() {
    TallyProbe::from_args().run();
}
