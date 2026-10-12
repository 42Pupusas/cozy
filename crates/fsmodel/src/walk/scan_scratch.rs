use std::path::Path;

use ququmatz::dir::{Dir, DirentBuf};
use ququmatz::{DirFd, OpenFlags};

use super::c_path::CPath;
use super::name_buffers::NameBuffers;

/// Per-thread working memory for [`DirScan`](super::DirScan).
///
/// Holds the `getdents64` record buffer, the C copy of the directory path,
/// and spare buffers for names split off into chunks. Allocated once and
/// reused, so a scan allocates only for what it hands back.
#[derive(Default)]
pub struct ScanScratch {
    pub(super) records: Box<DirentBuf>,
    pub(super) names: NameBuffers,
    path: CPath,
}

impl ScanScratch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            records: Box::new(DirentBuf::new()),
            names: NameBuffers::new(),
            path: CPath::new(),
        }
    }

    pub(super) fn open(&mut self, dir: &Path) -> Option<Dir> {
        let c_dir = self.path.of(dir)?;
        Dir::open(DirFd::Cwd, c_dir, OpenFlags::default()).ok()
    }
}

impl std::fmt::Debug for ScanScratch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanScratch").finish_non_exhaustive()
    }
}
