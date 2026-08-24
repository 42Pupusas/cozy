//! [`SystemTables`] -- the three kernel tables this crate reads, as one value.
//!
//! Detection needs three unrelated paths: the mount table, the sysfs block
//! directory, and the by-label symlink farm. Passing them individually was
//! the first design, and it had a specific failure: a caller could redirect
//! the mount table to a fixture while `/sys` still pointed at the live
//! machine. Every fixture device then classified as virtual and disappeared,
//! so a test comparing two empty sets passed while asserting nothing.
//!
//! Binding the three together makes that mistake unrepresentable: a test
//! either uses the whole fake system or the whole real one.

use crate::mount_table::MountTable;
use crate::volume_set::VolumeSet;
use std::path::{Path, PathBuf};

/// Where to read the kernel's storage tables from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemTables {
    /// The mount table, normally `/proc/self/mountinfo`.
    mountinfo: PathBuf,
    /// The block device directory, normally `/sys/class/block`.
    sys_block: PathBuf,
    /// The filesystem label links, normally `/dev/disk/by-label`.
    by_label: PathBuf,
}

impl SystemTables {
    /// Where the kernel publishes filesystem labels.
    pub const BY_LABEL: &'static str = "/dev/disk/by-label";

    /// The real tables of the running system.
    #[must_use]
    pub fn live() -> Self {
        Self {
            mountinfo: PathBuf::from(MountTable::PATH),
            sys_block: PathBuf::from(crate::DeviceClass::SYS_BLOCK),
            by_label: PathBuf::from(Self::BY_LABEL),
        }
    }

    /// A fake system rooted at `dir`, laid out as `TempDir` builds it.
    ///
    /// One constructor rather than three arguments, so a fixture cannot be
    /// half real.
    #[must_use]
    pub fn rooted_at(dir: &Path) -> Self {
        Self {
            mountinfo: dir.join("mountinfo"),
            sys_block: dir.join("class/block"),
            by_label: dir.join("by-label"),
        }
    }

    /// The mount table path.
    #[must_use]
    pub fn mountinfo(&self) -> &Path {
        &self.mountinfo
    }

    /// The raw mount table text, empty when unreadable.
    #[must_use]
    pub fn read_text(&self) -> String {
        MountTable::read_text_from(&self.mountinfo)
    }

    /// The listable volumes, read fresh from these tables.
    #[must_use]
    pub fn volumes(&self) -> VolumeSet {
        self.volumes_of(&self.read_text())
    }

    /// The listable volumes described by already-read mount table text.
    ///
    /// Split from [`Self::volumes`] so a poller that has just read the text
    /// for its change test does not read it a second time.
    #[must_use]
    pub fn volumes_of(&self, text: &str) -> VolumeSet {
        VolumeSet::from_table_in(&MountTable::parse(text), &self.sys_block, &self.by_label)
    }
}

impl Default for SystemTables {
    fn default() -> Self {
        Self::live()
    }
}

#[cfg(test)]
mod tests {
    use super::SystemTables;
    use crate::test_support::{fixtures, TempDir};
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";

    #[test]
    fn the_live_tables_point_at_the_real_kernel_paths() {
        let tables = SystemTables::live();
        assert_eq!(tables.mountinfo(), Path::new("/proc/self/mountinfo"));
        assert!(!tables.read_text().is_empty(), "the live table is readable");
    }

    /// The bug this type exists to prevent: a fixture mount table read
    /// against the live `/sys` classifies every fake device as virtual, and
    /// the resulting empty set makes assertions pass vacuously.
    #[test]
    fn a_rooted_system_resolves_its_own_devices() {
        let tmp = TempDir::new("tables-rooted");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.write("mountinfo", fixtures::DESKTOP);
        let volumes = SystemTables::rooted_at(tmp.path()).volumes();
        assert!(
            volumes.at(Path::new("/run/media/user/USB")).is_some(),
            "the fixture stick must resolve through the fixture sysfs"
        );
    }

    #[test]
    fn a_missing_mount_table_yields_no_volumes() {
        let tmp = TempDir::new("tables-missing");
        let tables = SystemTables::rooted_at(tmp.path());
        assert!(tables.read_text().is_empty());
        assert!(tables.volumes().is_empty());
    }

    /// Reading text once and reusing it must give the same answer as reading
    /// it again, or the poller's optimisation would change behaviour.
    #[test]
    fn parsing_read_text_matches_reading_directly() {
        let tmp = TempDir::new("tables-reuse");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.write("mountinfo", fixtures::DESKTOP);
        let tables = SystemTables::rooted_at(tmp.path());
        assert_eq!(tables.volumes(), tables.volumes_of(&tables.read_text()));
    }
}
