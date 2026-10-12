use std::collections::HashSet;

use super::dir_contents::DirContents;

/// The running total of one root's walk: bytes so far, hard-linked inodes
/// already counted, and how many directories and name chunks are still
/// unscanned.
#[derive(Debug)]
pub struct RootLedger {
    outstanding: usize,
    bytes: u64,
    linked: HashSet<(u64, u64)>,
}

impl RootLedger {
    #[must_use]
    pub fn new() -> Self {
        Self {
            outstanding: 1,
            bytes: 0,
            linked: HashSet::new(),
        }
    }

    /// A ledger already finished at `bytes`, for a root that is not a
    /// directory.
    #[must_use]
    pub fn settled(bytes: u64) -> Self {
        Self {
            outstanding: 0,
            bytes,
            linked: HashSet::new(),
        }
    }

    pub fn absorb(&mut self, contents: &DirContents) {
        self.outstanding =
            self.outstanding - 1 + contents.subdirs().len() + contents.chunks().len();
        self.bytes = self.bytes.saturating_add(contents.bytes());
        for linked in contents.linked() {
            if self.linked.insert(linked.key()) {
                self.bytes = self.bytes.saturating_add(linked.bytes);
            }
        }
    }

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    #[must_use]
    pub const fn outstanding(&self) -> usize {
        self.outstanding
    }

    #[must_use]
    pub const fn is_done(&self) -> bool {
        self.outstanding == 0
    }
}

impl Default for RootLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::RootLedger;
    use crate::walk::{DirContents, LinkedInode};
    use std::path::PathBuf;

    fn contents(bytes: u64, subdirs: usize, linked: &[LinkedInode]) -> DirContents {
        let mut contents = DirContents::new();
        contents.add_bytes(bytes);
        for index in 0..subdirs {
            contents.push_subdir(PathBuf::from(format!("d{index}")));
        }
        for &inode in linked {
            contents.push_linked(inode);
        }
        contents
    }

    const fn inode(inode: u64, bytes: u64) -> LinkedInode {
        LinkedInode {
            device: 1,
            inode,
            bytes,
        }
    }

    #[test]
    fn a_fresh_ledger_waits_for_its_root() {
        let ledger = RootLedger::new();
        assert_eq!(ledger.outstanding(), 1);
        assert!(!ledger.is_done());
        assert_eq!(ledger.bytes(), 0);
    }

    #[test]
    fn subdirectories_keep_the_ledger_open_until_each_is_absorbed() {
        let mut ledger = RootLedger::new();
        ledger.absorb(&contents(10, 2, &[]));
        assert_eq!(ledger.outstanding(), 2);
        ledger.absorb(&contents(5, 0, &[]));
        assert!(!ledger.is_done());
        ledger.absorb(&contents(1, 0, &[]));
        assert!(ledger.is_done());
        assert_eq!(ledger.bytes(), 16);
    }

    #[test]
    fn chunks_keep_the_ledger_open_like_subdirectories() {
        let mut level = DirContents::new();
        let mut packer = crate::walk::chunk_packer::ChunkPacker::new(std::path::Path::new("/d"), 1);
        let mut buffers = crate::walk::name_buffers::NameBuffers::new();
        packer.push(c"x", &mut buffers, &mut level);
        packer.finish(&mut level);
        let mut ledger = RootLedger::new();
        ledger.absorb(&level);
        assert_eq!(ledger.outstanding(), 1);
        ledger.absorb(&contents(4, 0, &[]));
        assert!(ledger.is_done());
    }

    #[test]
    fn a_settled_ledger_is_done_at_its_bytes() {
        let ledger = RootLedger::settled(9);
        assert!(ledger.is_done());
        assert_eq!(ledger.bytes(), 9);
    }

    #[test]
    fn an_inode_seen_in_two_directories_counts_once() {
        let mut ledger = RootLedger::new();
        ledger.absorb(&contents(0, 1, &[inode(7, 100), inode(8, 3)]));
        ledger.absorb(&contents(0, 0, &[inode(7, 100)]));
        assert_eq!(ledger.bytes(), 103);
    }
}
