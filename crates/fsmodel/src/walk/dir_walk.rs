//! [`DirWalk`] -- the recursive byte total of one directory, as `du -sbx`
//! reports it: apparent sizes, symlinks not followed, other filesystems not
//! entered, and each hard-linked inode counted once.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy)]
pub struct DirWalk<'a> {
    cancel: &'a AtomicBool,
}

impl<'a> DirWalk<'a> {
    #[must_use]
    pub const fn new(cancel: &'a AtomicBool) -> Self {
        Self { cancel }
    }

    /// The total bytes of every non-directory under `root`, or `None` if the
    /// walk was cancelled. Unreadable subdirectories contribute nothing
    /// rather than failing the whole total.
    #[must_use]
    pub fn total(&self, root: &Path) -> Option<u64> {
        let Ok(meta) = std::fs::symlink_metadata(root) else {
            return Some(0);
        };
        if !meta.is_dir() {
            return Some(meta.len());
        }
        let device = meta.dev();
        let mut linked = HashSet::new();
        let mut pending = vec![root.to_path_buf()];
        let mut total = 0_u64;
        while let Some(dir) = pending.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                if self.cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let Ok(meta) = item.metadata() else {
                    continue;
                };
                if meta.is_dir() {
                    if meta.dev() == device {
                        pending.push(item.path());
                    }
                } else if meta.nlink() < 2 || linked.insert((meta.dev(), meta.ino())) {
                    total = total.saturating_add(meta.len());
                }
            }
        }
        Some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::DirWalk;
    use crate::test_support::TempDir;
    use std::sync::atomic::AtomicBool;

    fn total(path: &std::path::Path) -> Option<u64> {
        let cancel = AtomicBool::new(false);
        DirWalk::new(&cancel).total(path)
    }

    #[test]
    fn nested_files_are_summed() {
        let dir = TempDir::new("walk_nested");
        dir.create_file("a", &[0; 5]);
        dir.create_file("sub/b", &[0; 7]);
        dir.create_file("sub/deeper/c", &[0; 11]);
        assert_eq!(total(dir.path()), Some(23));
    }

    #[test]
    fn an_empty_directory_totals_zero() {
        let dir = TempDir::new("walk_empty");
        dir.create_dir("hollow/inner");
        assert_eq!(total(dir.path()), Some(0));
    }

    #[test]
    fn a_symlinked_directory_is_not_followed() {
        let dir = TempDir::new("walk_symlink");
        dir.create_file("real/big", &[0; 1000]);
        dir.create_dir("probe");
        let link = dir.path().join("probe/link");
        std::os::unix::fs::symlink(dir.path().join("real"), &link).expect("symlink");
        let link_len = std::fs::symlink_metadata(&link).expect("stat").len();
        assert_eq!(total(&dir.path().join("probe")), Some(link_len));
    }

    #[test]
    fn hard_links_are_counted_once() {
        let dir = TempDir::new("walk_hardlink");
        let original = dir.create_file("one", &[0; 100]);
        std::fs::hard_link(&original, dir.path().join("two")).expect("hard link");
        assert_eq!(total(dir.path()), Some(100));
    }

    #[test]
    fn a_file_root_is_its_own_size() {
        let dir = TempDir::new("walk_file_root");
        let file = dir.create_file("f", &[0; 9]);
        assert_eq!(total(&file), Some(9));
    }

    #[test]
    fn a_missing_root_totals_zero() {
        let dir = TempDir::new("walk_missing");
        assert_eq!(total(&dir.path().join("gone")), Some(0));
    }

    #[test]
    fn a_cancelled_walk_reports_nothing() {
        let dir = TempDir::new("walk_cancel");
        dir.create_file("a", b"x");
        let cancel = AtomicBool::new(true);
        assert_eq!(DirWalk::new(&cancel).total(dir.path()), None);
    }
}
