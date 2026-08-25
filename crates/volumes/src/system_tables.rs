//! [`SystemTables`] -- the kernel tables this crate reads, as one value.
//!
//! Detection needs several unrelated paths: the mount table, the sysfs block
//! directory, the by-label symlink farm, udev's property cache, and the
//! kernel's block-device list. Passing them individually was the first
//! design, and it had a specific failure: a caller could redirect the mount
//! table to a fixture while `/sys` still pointed at the live machine. Every
//! fixture device then classified as virtual and disappeared, so a test
//! comparing two empty sets passed while asserting nothing.
//!
//! Binding them together makes that mistake unrepresentable: a test either
//! uses the whole fake system or the whole real one.

use crate::device_scan::DeviceScan;
use crate::mount_table::MountTable;
use crate::udev_properties::UdevProperties;
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
    /// udev's property cache, normally `/run/udev/data`.
    udev_root: PathBuf,
    /// The kernel's block device list, normally `/proc/partitions`.
    partitions: PathBuf,
}

impl SystemTables {
    /// Where the kernel publishes filesystem labels.
    pub const BY_LABEL: &'static str = "/dev/disk/by-label";

    /// Where the kernel lists every block device it knows.
    ///
    /// The cheap signal that a device was plugged in or pulled out. A stick
    /// that is attached and *not* mounted changes nothing in the mount
    /// table, so [`Self::read_text`] alone cannot see it appear.
    pub const PARTITIONS: &'static str = "/proc/partitions";

    /// The real tables of the running system.
    #[must_use]
    pub fn live() -> Self {
        Self {
            mountinfo: PathBuf::from(MountTable::PATH),
            sys_block: PathBuf::from(crate::DeviceClass::SYS_BLOCK),
            by_label: PathBuf::from(Self::BY_LABEL),
            udev_root: PathBuf::from(UdevProperties::DEFAULT_ROOT),
            partitions: PathBuf::from(Self::PARTITIONS),
        }
    }

    /// A fake system rooted at `dir`, laid out as `TempDir` builds it.
    ///
    /// One constructor rather than five arguments, so a fixture cannot be
    /// half real.
    #[must_use]
    pub fn rooted_at(dir: &Path) -> Self {
        Self {
            mountinfo: dir.join("mountinfo"),
            sys_block: dir.join("class/block"),
            by_label: dir.join("by-label"),
            udev_root: dir.join("data"),
            partitions: dir.join("partitions"),
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

    /// The raw text a device watcher compares between polls.
    ///
    /// Both tables, because a device changes state in two independent ways
    /// and each is invisible in the other's file:
    ///
    /// * Plugging a stick in rewrites `/proc/partitions` and, while it stays
    ///   unmounted, touches the mount table not at all.
    /// * Mounting one rewrites the mount table and leaves `/proc/partitions`
    ///   exactly as it was.
    ///
    /// Watching only the first would leave a freshly mounted stick still
    /// offering a Mount action; watching only the second is the gap that
    /// made an unmounted stick invisible in the first place.
    #[must_use]
    pub fn read_device_text(&self) -> String {
        let mut text = std::fs::read_to_string(&self.partitions).unwrap_or_default();
        text.push('\0');
        text.push_str(&self.read_text());
        text
    }

    /// Every removable device attached, mounted or not.
    #[must_use]
    pub fn devices(&self) -> DeviceScan {
        DeviceScan::scan_in(
            &self.sys_block,
            &self.udev_root,
            &MountTable::parse(&self.read_text()),
        )
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

    /// The gap that made an unmounted stick invisible: plugging a device in
    /// without mounting it changes nothing in the mount table, so a watcher
    /// comparing only that text would never poll again.
    #[test]
    fn attaching_an_unmounted_device_changes_the_device_text() {
        let tmp = TempDir::new("tables-attach");
        tmp.write("mountinfo", fixtures::DESKTOP);
        tmp.write("partitions", "major minor  #blocks  name\n 259  1  524288 nvme0n1p1\n");
        let tables = SystemTables::rooted_at(tmp.path());
        let mounts_before = tables.read_text();
        let before = tables.read_device_text();
        tmp.write(
            "partitions",
            "major minor  #blocks  name\n 259  1  524288 nvme0n1p1\n   8 17  976896 sdb1\n",
        );
        assert_eq!(
            tables.read_text(),
            mounts_before,
            "an unmounted device leaves the mount table untouched"
        );
        assert_ne!(before, tables.read_device_text(), "the new device must show");
    }

    /// The mirror: mounting a device rewrites the mount table and leaves
    /// `/proc/partitions` alone, so that half must be watched too.
    #[test]
    fn mounting_a_device_changes_the_device_text() {
        let tmp = TempDir::new("tables-mount");
        tmp.write("partitions", "major minor  #blocks  name\n   8 17  976896 sdb1\n");
        tmp.write("mountinfo", fixtures::DESKTOP_WITHOUT_USB);
        let tables = SystemTables::rooted_at(tmp.path());
        let before = tables.read_device_text();
        tmp.write("mountinfo", fixtures::DESKTOP);
        assert_ne!(before, tables.read_device_text());
    }

    /// A machine running `mdev`, or one with no `/proc` mounted, must read as
    /// "no devices" rather than panicking.
    #[test]
    fn missing_tables_yield_empty_device_text_and_no_devices() {
        let tmp = TempDir::new("tables-nodev");
        let tables = SystemTables::rooted_at(tmp.path());
        assert_eq!(tables.read_device_text(), "\0");
        assert!(tables.devices().is_empty());
    }

    /// The scan must run against the *fixture* sysfs and udev cache, or every
    /// fake device classifies as internal and the assertions pass vacuously.
    #[test]
    fn a_rooted_system_scans_its_own_unmounted_devices() {
        let tmp = TempDir::new("tables-scan");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
        let found = SystemTables::rooted_at(tmp.path()).devices();
        assert_eq!(found.len(), 1);
        assert!(found.devices()[0].is_mountable());
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
