use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use super::report_gate::ReportGate;
use super::scan_job::Work;
use super::tallied::Tallied;
use crate::walk::{DirContents, DirScan, RootLedger};

/// One root being totalled: its ledger, the work it still needs, and how
/// much of its total has been reported.
#[derive(Debug)]
pub struct RootWalk {
    path: PathBuf,
    scan: DirScan,
    ledger: RootLedger,
    gate: ReportGate,
    pending: Vec<Work>,
    finished: bool,
}

impl RootWalk {
    /// Start totalling `path`. A missing root totals zero and a
    /// non-directory its own length, both settled at once.
    #[must_use]
    pub fn open(path: PathBuf, chunk: usize) -> Self {
        let meta = std::fs::symlink_metadata(&path).ok();
        let device = meta.as_ref().map_or(0, MetadataExt::dev);
        let scan = DirScan::new(device).split_after(chunk);
        let (ledger, pending) = match meta {
            Some(meta) if meta.is_dir() => (RootLedger::new(), vec![Work::Dir(path.clone())]),
            Some(meta) => (RootLedger::settled(meta.len()), Vec::new()),
            None => (RootLedger::settled(0), Vec::new()),
        };
        Self {
            path,
            scan,
            ledger,
            gate: ReportGate::new(),
            pending,
            finished: false,
        }
    }

    #[must_use]
    pub const fn scan(&self) -> DirScan {
        self.scan
    }

    #[must_use]
    pub const fn has_work(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn take_work(&mut self) -> Option<Work> {
        self.pending.pop()
    }

    /// Fold in one finished piece of work, queueing what it found.
    pub fn absorb(&mut self, contents: &mut DirContents) {
        self.ledger.absorb(contents);
        self.pending.extend(contents.drain_subdirs().map(Work::Dir));
        self.pending
            .extend(contents.drain_chunks().map(Work::Chunk));
    }

    /// The report due now, if any: the final total once, or a partial
    /// total when it has grown enough since the last one.
    pub fn report(&mut self) -> Option<Tallied> {
        if self.finished {
            return None;
        }
        let bytes = self.ledger.bytes();
        let settled = self.ledger.is_done();
        if !settled && !self.gate.admit(bytes) {
            return None;
        }
        self.finished = settled;
        Some(Tallied {
            path: self.path.clone(),
            bytes,
            settled,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RootWalk;
    use crate::test_support::TempDir;

    #[test]
    fn a_file_root_settles_at_its_length_and_reports_once() {
        let dir = TempDir::new("root_walk_file");
        let file = dir.create_file("f", &[0; 9]);
        let mut walk = RootWalk::open(file, 4);
        assert!(!walk.has_work());
        let report = walk.report().expect("a final report");
        assert!(report.settled);
        assert_eq!(report.bytes, 9);
        assert_eq!(walk.report(), None);
    }

    #[test]
    fn a_missing_root_settles_at_zero() {
        let dir = TempDir::new("root_walk_missing");
        let mut walk = RootWalk::open(dir.path().join("gone"), 4);
        assert_eq!(walk.report().map(|report| report.bytes), Some(0));
    }

    #[test]
    fn a_directory_root_starts_with_itself_as_work() {
        let dir = TempDir::new("root_walk_dir");
        let mut walk = RootWalk::open(dir.path().to_path_buf(), 4);
        assert!(walk.has_work());
        assert_eq!(walk.report(), None, "nothing measured yet");
    }
}
