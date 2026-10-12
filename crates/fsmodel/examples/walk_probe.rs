//! Walks a real tree repeatedly so `perf` sees only the scan.
//!
//! `walk_probe <dir> [rounds] [ledger|std]`

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use fsmodel::walk::{DirContents, DirScan, DirWalk, RootLedger, ScanScratch};

#[derive(Clone, Copy)]
enum Engine {
    Ledger,
    Std,
}

struct WalkProbe {
    root: PathBuf,
    rounds: u32,
    engine: Engine,
    contents: DirContents,
    scratch: ScanScratch,
    pending: Vec<PathBuf>,
    dirs: u64,
}

impl WalkProbe {
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        let root = PathBuf::from(
            args.next()
                .expect("usage: walk_probe <dir> [rounds] [ledger|std]"),
        );
        let rounds = args.next().map_or(10, |raw| raw.parse().expect("rounds"));
        let engine = match args.next().as_deref() {
            None | Some("ledger") => Engine::Ledger,
            Some("std") => Engine::Std,
            Some(other) => panic!("unknown engine {other}"),
        };
        Self {
            root,
            rounds,
            engine,
            contents: DirContents::new(),
            scratch: ScanScratch::new(),
            pending: Vec::new(),
            dirs: 0,
        }
    }

    fn run(&mut self) {
        let mut best = Duration::MAX;
        let mut bytes = 0;
        let started = Instant::now();
        for _ in 0..self.rounds {
            let round = Instant::now();
            bytes = match self.engine {
                Engine::Ledger => self.ledger_total(),
                Engine::Std => Self::std_total(&self.root),
            };
            best = best.min(round.elapsed());
        }
        let mean = started.elapsed() / self.rounds.max(1);
        println!(
            "{} bytes, {} dirs/round, best {best:?}, mean {mean:?}",
            bytes,
            self.dirs / u64::from(self.rounds.max(1)),
        );
    }

    fn ledger_total(&mut self) -> u64 {
        let device = std::fs::symlink_metadata(&self.root)
            .expect("stat root")
            .dev();
        let scan = DirScan::new(device);
        let mut ledger = RootLedger::new();
        self.pending.push(self.root.clone());
        while let Some(dir) = self.pending.pop() {
            self.dirs += 1;
            ledger.absorb(scan.scan(&dir, &mut self.scratch, &mut self.contents));
            self.pending.extend(self.contents.drain_subdirs());
        }
        ledger.bytes()
    }

    fn std_total(root: &Path) -> u64 {
        let cancel = AtomicBool::new(false);
        DirWalk::new(&cancel).total(root).unwrap_or(0)
    }
}

fn main() {
    WalkProbe::from_args().run();
}
