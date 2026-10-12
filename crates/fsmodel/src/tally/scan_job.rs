use std::path::PathBuf;

use crate::walk::{DirContents, DirScan, NameChunk};

/// One unit of scanning: a whole directory level, or a chunk of names split
/// off a large one.
#[derive(Debug)]
pub enum Work {
    Dir(PathBuf),
    Chunk(NameChunk),
}

/// Work sent to a scan worker, carrying a recycled [`DirContents`] for the
/// worker to fill and send back.
#[derive(Debug)]
pub struct ScanJob {
    pub root: usize,
    pub scan: DirScan,
    pub work: Work,
    pub contents: DirContents,
}

/// A finished [`ScanJob`]: what the work found, for the root it belongs to.
#[derive(Debug)]
pub struct Scanned {
    pub root: usize,
    pub contents: DirContents,
}
