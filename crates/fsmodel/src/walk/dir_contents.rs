use std::path::PathBuf;
use std::vec::Drain;

use super::linked_inode::LinkedInode;
use super::name_chunk::NameChunk;

/// What one directory holds, without descending.
///
/// That is the bytes of its single-link files, the subdirectories still to
/// visit, the multiply-linked files whose bytes depend on what else the walk
/// has seen, and any names left unstat'ed for another thread to finish.
/// Reused across scans so its vectors keep their capacity.
#[derive(Debug, Default)]
pub struct DirContents {
    bytes: u64,
    subdirs: Vec<PathBuf>,
    linked: Vec<LinkedInode>,
    chunks: Vec<NameChunk>,
}

impl DirContents {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bytes: 0,
            subdirs: Vec::new(),
            linked: Vec::new(),
            chunks: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.bytes = 0;
        self.subdirs.clear();
        self.linked.clear();
        self.chunks.clear();
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub fn subdirs(&self) -> &[PathBuf] {
        &self.subdirs
    }

    #[must_use]
    pub fn linked(&self) -> &[LinkedInode] {
        &self.linked
    }

    #[must_use]
    pub fn chunks(&self) -> &[NameChunk] {
        &self.chunks
    }

    pub fn drain_subdirs(&mut self) -> Drain<'_, PathBuf> {
        self.subdirs.drain(..)
    }

    pub fn drain_chunks(&mut self) -> Drain<'_, NameChunk> {
        self.chunks.drain(..)
    }

    pub(crate) const fn add_bytes(&mut self, bytes: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
    }

    pub(crate) fn push_subdir(&mut self, dir: PathBuf) {
        self.subdirs.push(dir);
    }

    pub(crate) fn push_linked(&mut self, linked: LinkedInode) {
        self.linked.push(linked);
    }

    pub(crate) fn push_chunk(&mut self, chunk: NameChunk) {
        self.chunks.push(chunk);
    }
}
