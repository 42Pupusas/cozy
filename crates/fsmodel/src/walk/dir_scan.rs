use std::ffi::{CStr, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ququmatz::StatxFlags;
use ququmatz::dir::Dir;

use super::chunk_packer::ChunkPacker;
use super::dir_contents::DirContents;
use super::entry_stat::EntryStat;
use super::linked_inode::LinkedInode;
use super::name_chunk::NameChunk;
use super::scan_scratch::ScanScratch;

/// Reads one directory level into a [`DirContents`], staying on `device`.
/// An unreadable directory scans as empty.
///
/// Entries are listed with `getdents64` into the caller's [`ScanScratch`]
/// and stat'ed with `statx` relative to the open directory. With
/// [`split_after`](Self::split_after), names past the first `limit` are not
/// stat'ed but packed into [`NameChunk`]s of `limit` names, so other threads
/// can share the `statx` work of one large directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirScan {
    device: u64,
    limit: usize,
}

impl DirScan {
    #[must_use]
    pub const fn new(device: u64) -> Self {
        Self {
            device,
            limit: usize::MAX,
        }
    }

    /// Split each directory's names into chunks of `limit` once it holds
    /// more than `limit` entries.
    ///
    /// # Panics
    ///
    /// If `limit` is zero.
    #[must_use]
    pub const fn split_after(self, limit: usize) -> Self {
        assert!(limit > 0, "a chunk holds at least one name");
        Self { limit, ..self }
    }

    pub fn scan<'c>(
        &self,
        dir: &Path,
        scratch: &mut ScanScratch,
        contents: &'c mut DirContents,
    ) -> &'c DirContents {
        contents.clear();
        let Some(handle) = scratch.open(dir) else {
            return contents;
        };
        let mut packer = ChunkPacker::new(dir, self.limit);
        let mut seen = 0_usize;
        while let Ok(batch) = handle.read(&mut scratch.records) {
            if batch.is_empty() {
                break;
            }
            for entry in batch.filter(|entry| !entry.is_dot()) {
                if seen < self.limit {
                    self.absorb(&handle, dir, entry.name(), contents);
                } else {
                    packer.push(entry.name(), &mut scratch.names, contents);
                }
                seen += 1;
            }
        }
        packer.finish(contents);
        contents
    }

    /// `statx` every name in `chunk`, as [`scan`](Self::scan) would have.
    pub fn stat_chunk<'c>(
        &self,
        chunk: NameChunk,
        scratch: &mut ScanScratch,
        contents: &'c mut DirContents,
    ) -> &'c DirContents {
        contents.clear();
        if let Some(handle) = scratch.open(chunk.dir()) {
            for name in chunk.names() {
                self.absorb(&handle, chunk.dir(), name, contents);
            }
        }
        scratch.names.give(chunk.into_names());
        contents
    }

    fn absorb(self, handle: &Dir, dir: &Path, name: &CStr, contents: &mut DirContents) {
        let Some(stat) = handle
            .stat_child(name, StatxFlags::SYMLINK_NOFOLLOW, EntryStat::MASK)
            .ok()
            .as_ref()
            .and_then(EntryStat::of)
        else {
            return;
        };
        if stat.is_dir {
            if stat.device == self.device {
                contents.push_subdir(Self::child_path(dir, name));
            }
        } else if stat.links < 2 {
            contents.add_bytes(stat.bytes);
        } else {
            contents.push_linked(LinkedInode {
                device: stat.device,
                inode: stat.inode,
                bytes: stat.bytes,
            });
        }
    }

    fn child_path(dir: &Path, name: &CStr) -> PathBuf {
        let name = OsStr::from_bytes(name.to_bytes());
        let mut path = PathBuf::with_capacity(dir.as_os_str().len() + 1 + name.len());
        path.push(dir);
        path.push(name);
        path
    }
}

#[cfg(test)]
mod tests {
    use super::DirScan;
    use crate::test_support::TempDir;
    use crate::walk::{DirContents, ScanScratch};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    fn scan_of(dir: &Path) -> DirScan {
        DirScan::new(std::fs::metadata(dir).expect("stat").dev())
    }

    fn scanned(scan: DirScan, dir: &Path) -> DirContents {
        let mut contents = DirContents::new();
        scan.scan(dir, &mut ScanScratch::new(), &mut contents);
        contents
    }

    #[test]
    fn files_are_summed_and_subdirectories_listed_without_descending() {
        let dir = TempDir::new("scan_level");
        dir.create_file("a", &[0; 5]);
        dir.create_file("b", &[0; 7]);
        let sub = dir.create_dir("sub");
        dir.create_file("sub/deep", &[0; 1000]);
        let got = scanned(scan_of(dir.path()), dir.path());
        assert_eq!(got.bytes(), 12);
        assert_eq!(got.subdirs(), [sub]);
        assert!(got.linked().is_empty());
        assert!(got.chunks().is_empty());
    }

    #[test]
    fn hard_linked_files_are_reported_instead_of_summed() {
        let dir = TempDir::new("scan_linked");
        let original = dir.create_file("one", &[0; 100]);
        std::fs::hard_link(&original, dir.path().join("two")).expect("hard link");
        let got = scanned(scan_of(dir.path()), dir.path());
        assert_eq!(got.bytes(), 0);
        assert_eq!(got.linked().len(), 2);
        assert_eq!(got.linked()[0].key(), got.linked()[1].key());
        assert_eq!(got.linked()[0].bytes, 100);
    }

    #[test]
    fn linked_inodes_carry_the_same_device_and_inode_as_std() {
        let dir = TempDir::new("scan_linked_ids");
        let original = dir.create_file("one", &[0; 3]);
        std::fs::hard_link(&original, dir.path().join("two")).expect("hard link");
        let meta = std::fs::metadata(&original).expect("stat");
        let got = scanned(scan_of(dir.path()), dir.path());
        assert_eq!(got.linked()[0].key(), (meta.dev(), meta.ino()));
    }

    #[test]
    fn a_symlink_counts_as_its_own_length() {
        let dir = TempDir::new("scan_symlink");
        dir.create_file("real/big", &[0; 1000]);
        let link = dir.create_symlink("link", "real");
        let link_len = std::fs::symlink_metadata(&link).expect("stat").len();
        let got = scanned(scan_of(dir.path()), dir.path());
        assert_eq!(got.bytes(), link_len);
        assert_eq!(got.subdirs().len(), 1);
    }

    #[test]
    fn subdirectories_on_another_device_are_skipped() {
        let dir = TempDir::new("scan_device");
        dir.create_dir("sub");
        let device = std::fs::metadata(dir.path()).expect("stat").dev();
        let got = scanned(DirScan::new(device.wrapping_add(1)), dir.path());
        assert!(got.subdirs().is_empty());
    }

    #[test]
    fn a_directory_larger_than_one_batch_is_read_to_the_end() {
        let dir = TempDir::new("scan_many");
        for i in 0..2000 {
            dir.create_file(&format!("file-with-a-longish-name-{i:05}"), b"x");
        }
        let got = scanned(scan_of(dir.path()), dir.path());
        assert_eq!(got.bytes(), 2000);
    }

    #[test]
    fn a_missing_directory_scans_empty_and_clears_the_previous_scan() {
        let dir = TempDir::new("scan_missing");
        dir.create_file("a", &[0; 5]);
        dir.create_dir("sub");
        let scan = scan_of(dir.path());
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        scan.scan(dir.path(), &mut scratch, &mut contents);
        let got = scan.scan(&dir.path().join("gone"), &mut scratch, &mut contents);
        assert_eq!(got.bytes(), 0);
        assert!(got.subdirs().is_empty());
    }

    #[test]
    fn a_file_path_scans_empty() {
        let dir = TempDir::new("scan_file_path");
        let file = dir.create_file("f", &[0; 9]);
        let got = scanned(scan_of(dir.path()), &file);
        assert_eq!(got.bytes(), 0);
    }

    #[test]
    fn a_small_directory_is_never_split() {
        let dir = TempDir::new("scan_split_small");
        dir.create_file("a", &[0; 2]);
        dir.create_file("b", &[0; 3]);
        let got = scanned(scan_of(dir.path()).split_after(2), dir.path());
        assert_eq!(got.bytes(), 5);
        assert!(got.chunks().is_empty());
    }

    #[test]
    fn names_past_the_limit_are_chunked_and_their_stat_adds_up_to_the_whole() {
        let dir = TempDir::new("scan_split_large");
        for i in 0..10_u8 {
            dir.create_file(&format!("f{i}"), &vec![0; usize::from(i) + 1]);
        }
        dir.create_dir("sub");
        let scan = scan_of(dir.path()).split_after(3);
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        scan.scan(dir.path(), &mut scratch, &mut contents);
        assert_eq!(contents.chunks().len(), 3, "11 names: 3 inline, then 3+3+2");
        let chunks: Vec<_> = contents.drain_chunks().collect();
        let mut bytes = contents.bytes();
        let mut subdirs = contents.subdirs().len();
        let mut part = DirContents::new();
        for chunk in chunks {
            let got = scan.stat_chunk(chunk, &mut scratch, &mut part);
            bytes += got.bytes();
            subdirs += got.subdirs().len();
            assert!(got.chunks().is_empty());
        }
        assert_eq!(bytes, (1..=10).sum::<u64>());
        assert_eq!(subdirs, 1);
    }

    #[test]
    fn a_chunk_whose_directory_vanished_stats_empty() {
        let dir = TempDir::new("scan_split_gone");
        for i in 0..4 {
            dir.create_file(&format!("d/f{i}"), b"x");
        }
        let sub = dir.path().join("d");
        let scan = scan_of(dir.path()).split_after(1);
        let mut scratch = ScanScratch::new();
        let mut contents = DirContents::new();
        scan.scan(&sub, &mut scratch, &mut contents);
        let chunk = contents.drain_chunks().next().expect("a chunk");
        std::fs::remove_dir_all(&sub).expect("remove");
        assert_eq!(
            scan.stat_chunk(chunk, &mut scratch, &mut contents).bytes(),
            0
        );
    }
}
