//! [`DeviceScan`] -- every removable device present, mounted or not.
//!
//! This is the counterpart to [`VolumeSet`], which lists only what is
//! *mounted*. Together they are what a sidebar needs to show a stick and
//! offer to mount it.
//!
//! # The scan
//!
//! 1. Walk `/sys/class/block`, which lists every device the kernel knows.
//! 2. Keep the removable, non-empty ones ([`BlockDevice::is_candidate`]).
//! 3. Ask udev what filesystem each holds ([`UdevProperties`]).
//! 4. Mark the ones the mount table already accounts for.
//!
//! # Whole disks are dropped when their partitions are usable
//!
//! A partitioned stick appears in `/sys` **twice**: as `sdb` and as `sdb1`.
//! Both are removable and non-empty, so both survive the candidate test, and
//! listing both shows the user one device as two rows -- one of which cannot
//! be mounted usefully.
//!
//! The rule is: a whole disk is listed only when no partition of it is. That
//! keeps the unpartitioned case working, which is not rare -- a stick
//! formatted straight onto the device (`mkfs.vfat /dev/sdb`) has a filesystem
//! on the disk itself and no partitions at all.
//!
//! [`VolumeSet`]: crate::volume_set::VolumeSet

use crate::attached_device::AttachedDevice;
use crate::block_device::BlockDevice;
use crate::mount_table::MountTable;
use crate::udev_properties::UdevProperties;
use std::path::{Path, PathBuf};

/// Every removable device attached to the machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceScan {
    devices: Vec<AttachedDevice>,
}

impl DeviceScan {
    /// Scan the live system.
    #[must_use]
    pub fn scan() -> Self {
        Self::scan_in(
            Path::new(crate::DeviceClass::SYS_BLOCK),
            Path::new(UdevProperties::DEFAULT_ROOT),
            &MountTable::read(),
        )
    }

    /// Describe a set of devices directly, without scanning anything.
    ///
    /// The counterpart to [`crate::VolumeSet::of`], and public for the same
    /// reason: the sidebar tests live in the file browser and cannot conjure
    /// a `/sys` tree, a udev record and a mount table for a stick that is not
    /// plugged in.
    ///
    /// The order is preserved as given rather than re-sorted, so a test can
    /// state the display order it expects to see.
    #[must_use]
    pub const fn of(devices: Vec<AttachedDevice>) -> Self {
        Self { devices }
    }

    /// Scan arbitrary system tables, for testing.
    #[must_use]
    pub fn scan_in(sys_block: &Path, udev_root: &Path, mounts: &MountTable) -> Self {
        let candidates = Self::candidates(sys_block, udev_root);
        let partitioned = Self::disks_with_listed_partitions(&candidates);
        let mut devices: Vec<AttachedDevice> = candidates
            .into_iter()
            .filter(|(device, _)| !partitioned.contains(&device.name().to_owned()))
            .map(|(device, attached)| Self::locate(attached, &device, mounts))
            .collect();
        devices.sort_by(|a, b| {
            a.display_name()
                .cmp(b.display_name())
                .then(a.name().cmp(b.name()))
        });
        Self { devices }
    }

    /// Every device that is removable, present, and holds a filesystem.
    fn candidates(sys_block: &Path, udev_root: &Path) -> Vec<(BlockDevice, AttachedDevice)> {
        let Ok(entries) = std::fs::read_dir(sys_block) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(device) = BlockDevice::read(sys_block, &name) else {
                continue;
            };
            let properties =
                UdevProperties::read_block_in(udev_root, device.major(), device.minor());
            if let Some(attached) = AttachedDevice::describe(&device, &properties) {
                found.push((device, attached));
            }
        }
        found
    }

    /// The disks that have a listable partition, and so must not be listed
    /// themselves.
    ///
    /// A partition's name starts with its disk's name, which is how the
    /// kernel names them: `sdb1` on `sdb`, `mmcblk0p1` on `mmcblk0`. The
    /// check requires a real prefix relationship rather than a substring, so
    /// `sdb1` never claims to be part of `sdc`.
    fn disks_with_listed_partitions(candidates: &[(BlockDevice, AttachedDevice)]) -> Vec<String> {
        let disks: Vec<&str> = candidates
            .iter()
            .filter(|(device, _)| !device.is_partition())
            .map(|(device, _)| device.name())
            .collect();
        candidates
            .iter()
            .filter(|(device, _)| device.is_partition())
            .filter_map(|(device, _)| {
                disks
                    .iter()
                    .find(|disk| Self::is_partition_of(device.name(), disk))
                    .map(|disk| (*disk).to_owned())
            })
            .collect()
    }

    /// Whether `partition` is a partition of `disk` by kernel naming.
    fn is_partition_of(partition: &str, disk: &str) -> bool {
        partition.strip_prefix(disk).is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .trim_start_matches('p')
                    .chars()
                    .all(|c| c.is_ascii_digit())
        })
    }

    /// Attach the mount point, when the mount table knows this device.
    ///
    /// Matched on device numbers rather than names: a stick mounted through
    /// `/dev/disk/by-uuid/...` names no `/sys` node in the mount table, and
    /// a name comparison would call it unmounted and offer to mount it
    /// twice.
    fn locate(
        attached: AttachedDevice,
        device: &BlockDevice,
        mounts: &MountTable,
    ) -> AttachedDevice {
        let point = mounts
            .entries()
            .iter()
            .find(|entry| entry.major() == device.major() && entry.minor() == device.minor())
            .map(crate::MountEntry::mount_point);
        match point {
            Some(point) => attached.mounted_at(point),
            None => attached,
        }
    }

    /// The devices found, in display order.
    #[must_use]
    pub fn devices(&self) -> &[AttachedDevice] {
        &self.devices
    }

    /// How many devices are attached.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether nothing removable is attached.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// The devices that are not mounted, which a Mount action applies to.
    #[must_use]
    pub fn mountable(&self) -> Vec<&AttachedDevice> {
        self.devices
            .iter()
            .filter(|device| device.is_mountable())
            .collect()
    }

    /// The device backed by a `/dev` path.
    #[must_use]
    pub fn by_source(&self, source: &str) -> Option<&AttachedDevice> {
        self.devices.iter().find(|device| device.source() == source)
    }

    /// The mount points of every mounted device.
    #[must_use]
    pub fn mount_points(&self) -> Vec<PathBuf> {
        self.devices
            .iter()
            .filter_map(|device| device.mount_point().map(Path::to_path_buf))
            .collect()
    }

    /// Whether this scan differs from `previous` in any visible way.
    #[must_use]
    pub fn differs_from(&self, previous: &Self) -> bool {
        self.devices != previous.devices
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceScan;
    use crate::mount_table::MountTable;
    use crate::test_support::{TempDir, fixtures};
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SATA_BUS: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";

    /// A stick with one partition, known to udev as a `vfat` volume.
    fn stick_with_partition(tmp: &TempDir) {
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.sized(USB_BUS, "sdb", None, 8, 16, 1_960_000);
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
    }

    fn scan(tmp: &TempDir, mounts: &str) -> DeviceScan {
        DeviceScan::scan_in(
            &tmp.class_block(),
            &tmp.udev_data(),
            &MountTable::parse(mounts),
        )
    }

    /// The gap this whole module exists to close: a stick that is plugged in
    /// but not mounted is now representable, listable, and mountable.
    #[test]
    fn an_unmounted_stick_is_found_and_offered_for_mounting() {
        let tmp = TempDir::new("scan-unmounted");
        stick_with_partition(&tmp);
        let found = scan(&tmp, "");
        assert_eq!(found.len(), 1);
        let device = &found.devices()[0];
        assert_eq!(device.source(), "/dev/sdb1");
        assert_eq!(device.fs_type(), "vfat");
        assert!(!device.is_mounted());
        assert_eq!(found.mountable().len(), 1);
    }

    /// The same stick, once mounted, must stop offering the action.
    #[test]
    fn a_mounted_stick_is_found_but_not_offered_for_mounting() {
        let tmp = TempDir::new("scan-mounted");
        stick_with_partition(&tmp);
        let found = scan(
            &tmp,
            "40 25 8:17 / /run/media/user/EFI rw - vfat /dev/sdb1 rw",
        );
        assert_eq!(found.len(), 1);
        assert!(found.devices()[0].is_mounted());
        assert_eq!(
            found.devices()[0].mount_point(),
            Some(Path::new("/run/media/user/EFI"))
        );
        assert!(found.mountable().is_empty());
    }

    /// A device mounted through a `by-uuid` path names no `/sys` node in the
    /// mount table. Matching on the source string would call it unmounted
    /// and offer to mount an already-mounted stick.
    #[test]
    fn a_device_mounted_by_uuid_is_still_recognised_as_mounted() {
        let tmp = TempDir::new("scan-byuuid");
        stick_with_partition(&tmp);
        let found = scan(
            &tmp,
            "40 25 8:17 / /run/media/user/EFI rw - vfat /dev/disk/by-uuid/D7BC-1ECA rw",
        );
        assert!(
            found.devices()[0].is_mounted(),
            "the device numbers match even when the name does not"
        );
    }

    /// A partitioned stick appears in `/sys` as both `sdb` and `sdb1`.
    /// Listing both shows one device as two rows.
    #[test]
    fn a_partitioned_disk_is_not_listed_beside_its_partition() {
        let tmp = TempDir::new("scan-whole-disk");
        stick_with_partition(&tmp);
        tmp.udev_block(8, 16, "E:ID_FS_TYPE=vfat\nE:ID_FS_USAGE=filesystem\n");
        let found = scan(&tmp, "");
        assert_eq!(found.len(), 1, "one stick is one row");
        assert_eq!(found.devices()[0].name(), "sdb1");
    }

    /// A stick formatted straight onto the device has no partitions, and
    /// dropping whole disks unconditionally would hide it entirely.
    #[test]
    fn an_unpartitioned_stick_is_listed_as_the_whole_disk() {
        let tmp = TempDir::new("scan-superfloppy");
        tmp.block_disk(USB_BUS, "sdc", false);
        tmp.sized(USB_BUS, "sdc", None, 8, 32, 1_953_792);
        tmp.udev_block(8, 32, fixtures::UDEV_VFAT_PARTITION);
        let found = scan(&tmp, "");
        assert_eq!(found.len(), 1);
        assert_eq!(found.devices()[0].name(), "sdc");
    }

    /// Two separate sticks must both be listed; the prefix rule must not
    /// treat one as a partition of the other.
    #[test]
    fn two_separate_sticks_are_both_listed() {
        let tmp = TempDir::new("scan-two");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.sized(USB_BUS, "sdb", None, 8, 16, 1_000);
        tmp.udev_block(
            8,
            16,
            "E:ID_FS_LABEL=ALPHA\nE:ID_FS_TYPE=vfat\nE:ID_FS_USAGE=filesystem\n",
        );
        tmp.block_disk(USB_BUS, "sdc", false);
        tmp.sized(USB_BUS, "sdc", None, 8, 32, 1_000);
        tmp.udev_block(
            8,
            32,
            "E:ID_FS_LABEL=BETA\nE:ID_FS_TYPE=vfat\nE:ID_FS_USAGE=filesystem\n",
        );
        let found = scan(&tmp, "");
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn an_internal_disk_never_appears() {
        let tmp = TempDir::new("scan-internal");
        tmp.block_disk(SATA_BUS, "sda", false);
        tmp.block_partition(SATA_BUS, "sda", "sda1", 1);
        tmp.sized(SATA_BUS, "sda", Some("sda1"), 8, 1, 1_000_000);
        tmp.udev_block(8, 1, fixtures::UDEV_VFAT_PARTITION);
        assert!(scan(&tmp, "").is_empty());
    }

    #[test]
    fn a_machine_with_no_removable_media_scans_empty() {
        let tmp = TempDir::new("scan-none");
        assert!(scan(&tmp, "").is_empty());
        assert_eq!(scan(&tmp, "").len(), 0);
    }

    /// Under `mdev`, or before udev has processed a device, there is no
    /// property database at all. The scan must return nothing, not panic.
    #[test]
    fn a_missing_udev_database_yields_no_devices() {
        let tmp = TempDir::new("scan-noudev");
        stick_with_partition(&tmp);
        let found = DeviceScan::scan_in(
            &tmp.class_block(),
            &tmp.path().join("absent"),
            &MountTable::parse(""),
        );
        assert!(found.is_empty(), "an unidentifiable device is not offered");
    }

    #[test]
    fn a_device_is_found_by_its_dev_path() {
        let tmp = TempDir::new("scan-bysource");
        stick_with_partition(&tmp);
        let found = scan(&tmp, "");
        assert!(found.by_source("/dev/sdb1").is_some());
        assert!(found.by_source("/dev/sdz9").is_none());
    }

    /// Mounting a stick changes what the sidebar draws, so it must register
    /// as a difference even though the same device is present either way.
    #[test]
    fn mounting_a_device_is_a_difference() {
        let tmp = TempDir::new("scan-diff");
        stick_with_partition(&tmp);
        let before = scan(&tmp, "");
        let after = scan(
            &tmp,
            "40 25 8:17 / /run/media/user/EFI rw - vfat /dev/sdb1 rw",
        );
        assert_eq!(before.len(), after.len());
        assert!(after.differs_from(&before), "the mount state changed");
    }

    #[test]
    fn an_unchanged_system_reports_no_difference() {
        let tmp = TempDir::new("scan-stable");
        stick_with_partition(&tmp);
        assert!(!scan(&tmp, "").differs_from(&scan(&tmp, "")));
    }

    /// Kernel naming: `sdb1` belongs to `sdb`, `mmcblk0p1` to `mmcblk0`,
    /// and `sdb1` belongs to neither `sdc` nor `sd`.
    #[test]
    fn partition_ownership_follows_kernel_naming() {
        for (partition, disk) in [
            ("sdb1", "sdb"),
            ("mmcblk0p1", "mmcblk0"),
            ("nvme0n1p2", "nvme0n1"),
        ] {
            assert!(
                DeviceScan::is_partition_of(partition, disk),
                "{partition} belongs to {disk}"
            );
        }
        for (partition, disk) in [("sdb1", "sdc"), ("sdb1", "sdb1"), ("sdbb1", "sdb")] {
            assert!(
                !DeviceScan::is_partition_of(partition, disk),
                "{partition} does not belong to {disk}"
            );
        }
    }

    /// The live scan must not panic, and must never offer the root
    /// filesystem's disk as removable storage.
    #[test]
    fn the_live_scan_offers_nothing_holding_the_root_filesystem() {
        let found = DeviceScan::scan();
        let table = MountTable::read();
        let Some(root) = table.at("/") else {
            return;
        };
        for device in found.devices() {
            assert!(
                device.major() != root.major() || device.minor() != root.minor(),
                "{} holds the root filesystem",
                device.source()
            );
        }
    }
}
