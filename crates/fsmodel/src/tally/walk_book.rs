use std::collections::VecDeque;
use std::path::PathBuf;

use super::root_walk::RootWalk;
use super::scan_job::Work;
use super::tallied::Tallied;
use crate::walk::{DirContents, DirScan};

/// Every root of one tally, handing out work round-robin so each root keeps
/// progressing instead of one large tree holding up the rest.
#[derive(Debug)]
pub struct WalkBook {
    roots: Vec<RootWalk>,
    rota: VecDeque<usize>,
}

impl WalkBook {
    #[must_use]
    pub fn open(paths: Vec<PathBuf>, chunk: usize) -> Self {
        let roots: Vec<RootWalk> = paths
            .into_iter()
            .map(|path| RootWalk::open(path, chunk))
            .collect();
        let rota = (0..roots.len())
            .filter(|&index| roots[index].has_work())
            .collect();
        Self { roots, rota }
    }

    /// Reports for roots that settled without any scanning.
    pub fn settled_at_open(&mut self) -> impl Iterator<Item = Tallied> + '_ {
        self.roots
            .iter_mut()
            .filter(|root| !root.has_work())
            .filter_map(RootWalk::report)
    }

    /// The next piece of work, from the root whose turn it is.
    pub fn next(&mut self) -> Option<(usize, DirScan, Work)> {
        let index = self.rota.pop_front()?;
        let root = &mut self.roots[index];
        let work = root.take_work()?;
        if root.has_work() {
            self.rota.push_back(index);
        }
        Some((index, root.scan(), work))
    }

    /// Fold a finished piece of work into root `index`, returning the report
    /// it made due.
    pub fn absorb(&mut self, index: usize, contents: &mut DirContents) -> Option<Tallied> {
        let root = &mut self.roots[index];
        let had_work = root.has_work();
        root.absorb(contents);
        if !had_work && root.has_work() {
            self.rota.push_back(index);
        }
        root.report()
    }
}

#[cfg(test)]
mod tests {
    use super::WalkBook;
    use crate::test_support::TempDir;
    use crate::walk::{DirContents, ScanScratch};

    #[test]
    fn work_alternates_between_roots() {
        let dir = TempDir::new("walk_book_rota");
        let a = dir.create_dir("a");
        let b = dir.create_dir("b");
        dir.create_dir("a/one");
        dir.create_dir("a/two");
        dir.create_dir("b/one");
        let mut book = WalkBook::open(vec![a, b], 64);
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        let mut order = Vec::new();
        while let Some((index, scan, work)) = book.next() {
            order.push(index);
            let crate::tally::scan_job::Work::Dir(path) = work else {
                panic!("no chunks at this size");
            };
            scan.scan(&path, &mut scratch, &mut contents);
            book.absorb(index, &mut contents);
        }
        assert_eq!(
            order[..2],
            [0, 1],
            "each root gets a turn before either repeats"
        );
        assert_eq!(order.len(), 5);
    }

    #[test]
    fn roots_that_need_no_scanning_report_at_open() {
        let dir = TempDir::new("walk_book_files");
        let file = dir.create_file("f", &[0; 4]);
        let sub = dir.create_dir("d");
        let mut book = WalkBook::open(vec![file, sub], 64);
        let settled: Vec<_> = book.settled_at_open().collect();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].bytes, 4);
    }
}
