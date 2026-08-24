//! [`MountTable`] -- the whole mount list, read and reread.
//!
//! Separated from [`crate::MountEntry`] so the *source* of the text is a
//! seam: tests drive the parser with fixtures covering hardware this machine
//! does not have (a USB stick, an SD card, a read-only optical disc), while
//! production reads the live file.

use crate::mount_entry::MountEntry;
use std::path::{Path, PathBuf};

/// Every mount currently visible to this process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MountTable {
    entries: Vec<MountEntry>,
}

impl MountTable {
    /// Where the kernel publishes the mount table for the calling process.
    ///
    /// `self` rather than a fixed pid so the answer is correct inside a mount
    /// namespace, where the global table would describe a different world
    /// from the one this process can actually open.
    pub const PATH: &'static str = "/proc/self/mountinfo";

    /// Read the live mount table.
    ///
    /// An unreadable file yields an empty table rather than an error: the
    /// only caller is a sidebar section, and the honest response to "the
    /// kernel will not tell us" is to list no devices.
    #[must_use]
    pub fn read() -> Self {
        Self::read_from(Path::new(Self::PATH))
    }

    /// Read a mount table from an arbitrary path.
    #[must_use]
    pub fn read_from(path: &Path) -> Self {
        Self::parse(&Self::read_text_from(path))
    }

    /// The live mount table as raw text, before parsing.
    ///
    /// Exposed because a poller compares successive reads to decide whether
    /// to do any work at all. The kernel rewrites this file on every read,
    /// so its mtime never changes and byte comparison is the only cheap
    /// change test available.
    #[must_use]
    pub fn read_text() -> String {
        Self::read_text_from(Path::new(Self::PATH))
    }

    /// Read mount-table text from an arbitrary path, empty when unreadable.
    #[must_use]
    pub fn read_text_from(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Parse mount-table text.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        Self {
            entries: text.lines().filter_map(MountEntry::parse).collect(),
        }
    }

    /// Every parsed entry.
    #[must_use]
    pub fn entries(&self) -> &[MountEntry] {
        &self.entries
    }

    /// How many mounts were parsed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table holds no mounts.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entry mounted exactly at `path`.
    #[must_use]
    pub fn at(&self, path: &str) -> Option<&MountEntry> {
        self.entries
            .iter()
            .find(|entry| entry.mount_point() == path)
    }

    /// The mount that contains `path` -- the longest mount point that is a
    /// prefix of it.
    ///
    /// Longest-match, because mount points nest: `/home` sits inside `/`, and
    /// a file under `/home` belongs to the former. Prefix comparison is done
    /// component-wise so `/home` does not match `/homework`.
    #[must_use]
    pub fn containing(&self, path: &Path) -> Option<&MountEntry> {
        self.entries
            .iter()
            .filter(|entry| Self::is_ancestor(entry.mount_point(), path))
            .max_by_key(|entry| entry.mount_point().len())
    }

    /// Whether `mount_point` is `path` or one of its ancestors.
    fn is_ancestor(mount_point: &str, path: &Path) -> bool {
        let mount = PathBuf::from(mount_point);
        path.starts_with(&mount)
    }
}

#[cfg(test)]
mod tests {
    use super::MountTable;
    use crate::test_support::fixtures;
    use std::path::Path;

    #[test]
    fn parses_a_multi_line_table() {
        let table = MountTable::parse(fixtures::DESKTOP);
        assert!(table.len() > 3);
        assert!(!table.is_empty());
    }

    #[test]
    fn an_empty_table_is_reported_as_empty() {
        let table = MountTable::parse("");
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn finds_a_mount_by_its_exact_point() {
        let table = MountTable::parse(fixtures::DESKTOP);
        let usb = table.at("/run/media/user/USB").expect("the usb stick");
        assert_eq!(usb.fs_type(), "vfat");
        assert!(table.at("/nowhere").is_none());
    }

    /// Mount points nest, so the containing mount is the *longest* match.
    /// Taking the first match would attribute every file to `/`.
    #[test]
    fn the_containing_mount_is_the_longest_matching_point() {
        let table = MountTable::parse(fixtures::DESKTOP);
        let entry = table
            .containing(Path::new("/home/user/notes.txt"))
            .expect("a containing mount");
        assert_eq!(entry.mount_point(), "/home");
    }

    #[test]
    fn a_path_on_the_root_filesystem_resolves_to_root() {
        let table = MountTable::parse(fixtures::DESKTOP);
        let entry = table.containing(Path::new("/etc/fstab")).expect("root");
        assert_eq!(entry.mount_point(), "/");
    }

    /// String-prefix matching would say `/homework` is inside `/home`, and
    /// the browser would then report the wrong device and free space.
    #[test]
    fn a_similar_prefix_is_not_treated_as_a_containing_mount() {
        let table = MountTable::parse(fixtures::DESKTOP);
        let entry = table.containing(Path::new("/homework/file")).expect("root");
        assert_eq!(
            entry.mount_point(),
            "/",
            "/homework belongs to the root filesystem, not to /home"
        );
    }

    #[test]
    fn a_mount_point_itself_is_contained_by_its_own_mount() {
        let table = MountTable::parse(fixtures::DESKTOP);
        let entry = table.containing(Path::new("/home")).expect("home");
        assert_eq!(entry.mount_point(), "/home");
    }

    /// An unreadable path is a normal outcome, not an error to surface.
    #[test]
    fn an_unreadable_table_reads_as_empty() {
        let table = MountTable::read_from(Path::new("/nonexistent-mountinfo-for-tests"));
        assert!(table.is_empty());
    }

    /// The live file must parse on the machine running the tests.
    #[test]
    fn the_live_table_is_readable_and_has_a_root() {
        let table = MountTable::read();
        assert!(!table.is_empty(), "a running Linux system always has mounts");
        assert!(table.at("/").is_some(), "there is always a root mount");
    }
}
