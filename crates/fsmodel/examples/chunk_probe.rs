//! Checks whether one large directory's `statx` work scales when its names
//! are split across threads, each with its own descriptor on the directory.
//!
//! `chunk_probe <dir> [rounds] [max_threads]`

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ququmatz::dir::{Dir, DirentBuf};
use ququmatz::{DirFd, OpenFlags, StatxFlags, StatxMask};

struct NameChunk<'n> {
    dir: &'n CStr,
    names: &'n [CString],
}

impl NameChunk<'_> {
    const MASK: StatxMask = StatxMask::TYPE
        .union_const(StatxMask::NLINK)
        .union_const(StatxMask::INO)
        .union_const(StatxMask::SIZE);

    fn bytes(&self) -> u64 {
        let handle = Dir::open(DirFd::Cwd, self.dir, OpenFlags::default()).expect("open");
        self.names
            .iter()
            .filter_map(|name| {
                handle
                    .stat_child(name, StatxFlags::SYMLINK_NOFOLLOW, Self::MASK)
                    .ok()
            })
            .map(|stat| stat.stx_size)
            .sum()
    }
}

struct ChunkProbe {
    dir: CString,
    names: Vec<CString>,
    rounds: u32,
    max_threads: usize,
}

impl ChunkProbe {
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        let dir = PathBuf::from(
            args.next()
                .expect("usage: chunk_probe <dir> [rounds] [max_threads]"),
        );
        let rounds = args.next().map_or(10, |raw| raw.parse().expect("rounds"));
        let max_threads = args
            .next()
            .map_or(16, |raw| raw.parse().expect("max_threads"));
        let dir = CString::new(dir.as_os_str().as_bytes()).expect("nul in path");
        let names = Self::list(&dir);
        Self {
            dir,
            names,
            rounds,
            max_threads,
        }
    }

    fn list(dir: &CStr) -> Vec<CString> {
        let handle = Dir::open(DirFd::Cwd, dir, OpenFlags::default()).expect("open");
        let mut records = Box::new(DirentBuf::new());
        let mut names = Vec::new();
        while let Ok(batch) = handle.read(&mut records) {
            if batch.is_empty() {
                break;
            }
            names.extend(
                batch
                    .filter(|entry| !entry.is_dot())
                    .map(|entry| entry.name().to_owned()),
            );
        }
        names
    }

    fn round(&self, threads: usize) -> u64 {
        let per = self.names.len().div_ceil(threads);
        std::thread::scope(|scope| {
            let mut running = Vec::with_capacity(threads);
            for names in self.names.chunks(per) {
                let chunk = NameChunk {
                    dir: &self.dir,
                    names,
                };
                running.push(scope.spawn(move || chunk.bytes()));
            }
            running
                .into_iter()
                .map(|handle| handle.join().expect("chunk"))
                .sum()
        })
    }

    fn run(&self) {
        println!("{} names", self.names.len());
        let mut threads = 1;
        let mut baseline = Duration::ZERO;
        while threads <= self.max_threads {
            let mut best = Duration::MAX;
            let mut bytes = 0;
            for _ in 0..self.rounds {
                let start = Instant::now();
                bytes = self.round(threads);
                best = best.min(start.elapsed());
            }
            if threads == 1 {
                baseline = best;
            }
            println!(
                "{threads:>2} threads  best {best:>12?}  speedup {:>5.2}x  ({bytes} bytes)",
                baseline.as_secs_f64() / best.as_secs_f64()
            );
            threads *= 2;
        }
    }
}

fn main() {
    ChunkProbe::from_args().run();
}
