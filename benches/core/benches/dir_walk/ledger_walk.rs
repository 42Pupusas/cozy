use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use fsmodel::walk::{DirContents, DirScan, RootLedger, ScanScratch};

/// A serial walk built from [`DirScan`] and [`RootLedger`], the pieces the
/// worker pool will split across threads.
pub struct LedgerWalk {
    contents: DirContents,
    scratch: ScanScratch,
    pending: Vec<PathBuf>,
}

impl LedgerWalk {
    #[must_use]
    pub fn new() -> Self {
        Self {
            contents: DirContents::new(),
            scratch: ScanScratch::new(),
            pending: Vec::new(),
        }
    }

    pub fn total(&mut self, root: &Path) -> u64 {
        let device = std::fs::symlink_metadata(root).expect("stat root").dev();
        let scan = DirScan::new(device);
        let mut ledger = RootLedger::new();
        self.pending.clear();
        self.pending.push(root.to_path_buf());
        while let Some(dir) = self.pending.pop() {
            ledger.absorb(scan.scan(&dir, &mut self.scratch, &mut self.contents));
            self.pending.extend(self.contents.drain_subdirs());
        }
        assert!(ledger.is_done());
        ledger.bytes()
    }
}
