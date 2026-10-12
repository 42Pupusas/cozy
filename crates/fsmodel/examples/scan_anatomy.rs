//! Splits a serial walk's wall time by syscall: `openat`, `getdents64`,
//! `statx`, and `close`.
//!
//! `scan_anatomy <dir> [rounds]`

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ququmatz::dir::{Dir, DirEntryKind, DirentBuf};
use ququmatz::{DirFd, OpenFlags, StatxFlags, StatxMask};

#[derive(Default)]
struct Phase {
    calls: u64,
    spent: Duration,
}

impl Phase {
    fn time<T>(&mut self, op: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = op();
        self.spent += start.elapsed();
        self.calls += 1;
        out
    }

    fn report(&self, label: &str, rounds: u32, total: Duration) {
        let per_round = self.spent / rounds;
        let per_call = self
            .spent
            .checked_div(u32::try_from(self.calls).unwrap_or(u32::MAX))
            .unwrap_or_default();
        println!(
            "{label:<10} {:>8} calls/round  {per_round:>12?}/round  {per_call:>9?}/call  {:>5.1}%",
            self.calls / u64::from(rounds),
            100.0 * self.spent.as_secs_f64() / total.as_secs_f64(),
        );
    }
}

#[derive(Default)]
struct ScanAnatomy {
    open: Phase,
    read: Phase,
    stat_file: Phase,
    stat_dir: Phase,
    close: Phase,
    unknown_kind: u64,
    heaviest: Vec<(Duration, u64, PathBuf)>,
}

impl ScanAnatomy {
    fn walk(&mut self, root: PathBuf, records: &mut DirentBuf) {
        let mut pending = vec![root];
        while let Some(dir) = pending.pop() {
            let c_dir = CString::new(dir.as_os_str().as_bytes()).expect("nul in path");
            let dir_start = Instant::now();
            let mut entries = 0;
            let Ok(handle) = self
                .open
                .time(|| Dir::open(DirFd::Cwd, &c_dir, OpenFlags::default()))
            else {
                continue;
            };
            while let Ok(batch) = self.read.time(|| handle.read(records)) {
                if batch.is_empty() {
                    break;
                }
                for entry in batch.filter(|entry| !entry.is_dot()) {
                    entries += 1;
                    let kind = entry.kind();
                    let phase = match kind {
                        DirEntryKind::Directory => &mut self.stat_dir,
                        DirEntryKind::Unknown => {
                            self.unknown_kind += 1;
                            &mut self.stat_file
                        }
                        _ => &mut self.stat_file,
                    };
                    let stat = phase.time(|| {
                        handle.stat_child(entry.name(), StatxFlags::SYMLINK_NOFOLLOW, Self::MASK)
                    });
                    if kind == DirEntryKind::Directory && stat.is_ok() {
                        pending
                            .push(dir.join(std::ffi::OsStr::from_bytes(entry.name().to_bytes())));
                    }
                }
            }
            self.close.time(|| drop(handle));
            self.heaviest.push((dir_start.elapsed(), entries, dir));
        }
    }

    const MASK: StatxMask = StatxMask::TYPE
        .union_const(StatxMask::NLINK)
        .union_const(StatxMask::INO)
        .union_const(StatxMask::SIZE);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: scan_anatomy <dir> [rounds]"));
    let rounds: u32 = args.next().map_or(10, |raw| raw.parse().expect("rounds"));
    let mut records = Box::new(DirentBuf::new());
    let mut anatomy = ScanAnatomy::default();
    let started = Instant::now();
    for _ in 0..rounds {
        anatomy.walk(root.clone(), &mut records);
    }
    let total = started.elapsed();
    println!("total {:?}/round", total / rounds);
    anatomy.open.report("openat", rounds, total);
    anatomy.read.report("getdents", rounds, total);
    anatomy.stat_file.report("statx file", rounds, total);
    anatomy.stat_dir.report("statx dir", rounds, total);
    anatomy.close.report("close", rounds, total);
    println!(
        "d_type unknown: {}",
        anatomy.unknown_kind / u64::from(rounds)
    );
    let one_round = anatomy.heaviest.len() / rounds as usize;
    let mut last = anatomy
        .heaviest
        .split_off(anatomy.heaviest.len() - one_round);
    last.sort_by_key(|heavy| std::cmp::Reverse(heavy.0));
    let round_total: Duration = last.iter().map(|(spent, _, _)| *spent).sum();
    let mut running = Duration::ZERO;
    for (rank, (spent, entries, dir)) in last.iter().take(15).enumerate() {
        running += *spent;
        println!(
            "#{rank:<2} {spent:>10?} {entries:>6} entries  cum {:>5.1}%  {}",
            100.0 * running.as_secs_f64() / round_total.as_secs_f64(),
            dir.display()
        );
    }
    let mut by_entries: Vec<u64> = last.iter().map(|(_, entries, _)| *entries).collect();
    by_entries.sort_unstable();
    println!(
        "dirs {}  median entries {}  p99 {}  max {}",
        by_entries.len(),
        by_entries[by_entries.len() / 2],
        by_entries[by_entries.len() * 99 / 100],
        by_entries[by_entries.len() - 1]
    );
}
