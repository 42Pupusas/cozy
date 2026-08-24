//! [`VolumeSet`] -- every listable volume, and what changed since last time.
//!
//! Two jobs, because a sidebar section needs both:
//!
//! - **What to draw now**: the removable volumes, in a stable order.
//! - **Whether to redraw at all**: the window repaints on change, not on a
//!   clock, so a rescan that finds nothing new must be able to say so.
//!
//! The second job is why [`Self::differs_from`] exists rather than the caller
//! comparing lengths. A stick swapped for another between two polls leaves the
//! count identical while the content is entirely different.

use crate::device_class::DeviceClass;
use crate::mount_table::MountTable;
use crate::volume::Volume;
use std::path::{Path, PathBuf};

/// The removable volumes currently attached.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VolumeSet {
    volumes: Vec<Volume>,
}

impl VolumeSet {
    /// Scan the live system for removable volumes.
    #[must_use]
    pub fn scan() -> Self {
        Self::from_table(&MountTable::read())
    }

    /// Build the set from an already-read mount table.
    #[must_use]
    pub fn from_table(table: &MountTable) -> Self {
        Self::collect(table, Volume::from_mount)
    }

    /// Build the set against arbitrary system tables, for testing.
    #[must_use]
    pub fn from_table_in(table: &MountTable, sys_block: &Path, by_label: &Path) -> Self {
        Self::collect(table, |entry| {
            Volume::from_mount_in(entry, sys_block, by_label)
        })
    }

    /// Turn the mount table into the listable volumes, in display order.
    ///
    /// Pseudo-filesystems are rejected before any `/sys` lookup: that check
    /// is a string comparison, while classification resolves symlinks, and
    /// most of a real mount table is pseudo-filesystems.
    fn collect(table: &MountTable, build: impl Fn(&crate::MountEntry) -> Volume) -> Self {
        let mut volumes: Vec<Volume> = table
            .entries()
            .iter()
            .filter(|entry| entry.has_block_source())
            .filter(|entry| !DeviceClass::is_pseudo_filesystem(entry.fs_type()))
            .map(build)
            .filter(Volume::is_listable)
            .collect();
        volumes.sort_by(|a, b| a.label().cmp(b.label()).then(a.mount_point().cmp(b.mount_point())));
        volumes.dedup_by(|a, b| a.mount_point() == b.mount_point());
        Self { volumes }
    }

    /// The volumes to display.
    #[must_use]
    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    /// How many volumes are listed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.volumes.len()
    }

    /// Whether no volumes are attached.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.volumes.is_empty()
    }

    /// The volume mounted at `path`, if it is one of ours.
    #[must_use]
    pub fn at(&self, path: &Path) -> Option<&Volume> {
        self.volumes
            .iter()
            .find(|volume| volume.mount_point() == path)
    }

    /// The volume containing `path`, by longest matching mount point.
    ///
    /// What the storage footer asks when the user navigates into a device.
    #[must_use]
    pub fn containing(&self, path: &Path) -> Option<&Volume> {
        self.volumes
            .iter()
            .filter(|volume| path.starts_with(volume.mount_point()))
            .max_by_key(|volume| volume.mount_point().as_os_str().len())
    }

    /// The mount points, for cheap comparison and for tests.
    #[must_use]
    pub fn mount_points(&self) -> Vec<PathBuf> {
        self.volumes
            .iter()
            .map(|volume| volume.mount_point().to_path_buf())
            .collect()
    }

    /// Whether this set differs from `previous` in any user-visible way.
    ///
    /// Compares content, not length: swapping one stick for another keeps the
    /// count the same while changing everything the sidebar draws.
    #[must_use]
    pub fn differs_from(&self, previous: &Self) -> bool {
        self.volumes != previous.volumes
    }
}

#[cfg(test)]
mod tests {
    use super::VolumeSet;
    use crate::mount_table::MountTable;
    use crate::test_support::{fixtures, TempDir};
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SD_BUS: &str = "pci0000:00/0000:00:1c.0/mmc_host/mmc0/mmc0:0001";
    const SATA_BUS: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";
    const NVME_BUS: &str = "pci0000:00/0000:00:1d.0/0000:03:00.0/nvme/nvme0";

    /// A sysfs mirroring the `DESKTOP` fixture: internal `NVMe`, USB stick, SD
    /// card, and an optical drive.
    fn desktop_sysfs(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        tmp.block_disk(NVME_BUS, "nvme0n1", false);
        tmp.block_partition(NVME_BUS, "nvme0n1", "nvme0n1p1", 1);
        tmp.block_partition(NVME_BUS, "nvme0n1", "nvme0n1p2", 2);
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.block_disk(SD_BUS, "mmcblk0", false);
        tmp.block_partition(SD_BUS, "mmcblk0", "mmcblk0p1", 1);
        tmp.block_disk(SATA_BUS, "sr0", true);
        tmp
    }

    fn set_of(text: &str, tmp: &TempDir) -> VolumeSet {
        let table = MountTable::parse(text);
        VolumeSet::from_table_in(&table, &tmp.class_block(), &tmp.path().join("by-label"))
    }

    /// The whole point: from a 13-line mount table, exactly the three
    /// removable volumes appear.
    #[test]
    fn only_removable_volumes_are_listed() {
        let tmp = desktop_sysfs("set-desktop");
        let set = set_of(fixtures::DESKTOP, &tmp);
        let labels: Vec<&str> = set.volumes().iter().map(super::Volume::label).collect();
        assert_eq!(labels, ["AUDIO", "SDCARD", "USB"]);
    }

    /// The root filesystem, `/home` and `/boot` are real devices but not
    /// removable, and listing them would clutter the section.
    #[test]
    fn internal_partitions_are_excluded() {
        let tmp = desktop_sysfs("set-internal");
        let set = set_of(fixtures::DESKTOP, &tmp);
        for point in ["/", "/home", "/boot"] {
            assert!(
                set.at(Path::new(point)).is_none(),
                "{point} is internal and must not be listed"
            );
        }
    }

    /// This machine has three `AppImage` FUSE mounts right now. They name a
    /// source, so only the filesystem-type check keeps them out.
    #[test]
    fn fuse_and_pseudo_mounts_are_excluded() {
        let tmp = desktop_sysfs("set-fuse");
        let set = set_of(fixtures::DESKTOP, &tmp);
        assert!(set.at(Path::new("/tmp/.mount_appimg")).is_none());
        assert!(set.at(Path::new("/proc")).is_none());
        assert!(set.at(Path::new("/tmp")).is_none());
    }

    #[test]
    fn an_empty_system_lists_nothing() {
        let tmp = desktop_sysfs("set-empty");
        let set = set_of("", &tmp);
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn a_volume_is_found_by_its_mount_point() {
        let tmp = desktop_sysfs("set-at");
        let set = set_of(fixtures::DESKTOP, &tmp);
        let usb = set.at(Path::new("/run/media/user/USB")).expect("the stick");
        assert_eq!(usb.fs_type(), "vfat");
    }

    /// What the storage footer needs when the user descends into a device.
    #[test]
    fn a_path_inside_a_volume_resolves_to_it() {
        let tmp = desktop_sysfs("set-containing");
        let set = set_of(fixtures::DESKTOP, &tmp);
        let volume = set
            .containing(Path::new("/run/media/user/USB/photos/img.png"))
            .expect("the containing volume");
        assert_eq!(volume.label(), "USB");
    }

    #[test]
    fn a_path_outside_every_volume_resolves_to_nothing() {
        let tmp = desktop_sysfs("set-outside");
        let set = set_of(fixtures::DESKTOP, &tmp);
        assert!(set.containing(Path::new("/home/user/notes.txt")).is_none());
    }

    /// Unplugging a stick must register as a change so the sidebar redraws.
    #[test]
    fn removing_a_device_is_a_difference() {
        let tmp = desktop_sysfs("set-unplug");
        let before = set_of(fixtures::DESKTOP, &tmp);
        let after = set_of(fixtures::DESKTOP_WITHOUT_USB, &tmp);
        assert!(after.differs_from(&before));
        assert_eq!(after.len(), before.len() - 2, "the usb stick and the disc go");
    }

    /// A rescan finding the same hardware must NOT report a change, or the
    /// window repaints on every poll and the idle cost is permanent.
    #[test]
    fn an_unchanged_system_reports_no_difference() {
        let tmp = desktop_sysfs("set-stable");
        let before = set_of(fixtures::DESKTOP, &tmp);
        let after = set_of(fixtures::DESKTOP, &tmp);
        assert!(!after.differs_from(&before));
    }

    /// Swapping one stick for another between polls keeps the count the same
    /// while changing what the sidebar must draw. A length comparison would
    /// miss this entirely.
    #[test]
    fn swapping_one_device_for_another_is_a_difference_despite_an_equal_count() {
        let tmp = desktop_sysfs("set-swap");
        let first = set_of(
            "40 25 8:17 / /run/media/user/ALPHA rw - vfat /dev/sdb1 rw",
            &tmp,
        );
        let second = set_of(
            "40 25 8:17 / /run/media/user/BETA rw - vfat /dev/sdb1 rw",
            &tmp,
        );
        assert_eq!(first.len(), second.len());
        assert!(second.differs_from(&first));
    }

    /// Order must not depend on the mount table's order, which reflects mount
    /// sequence -- the section would otherwise reshuffle as devices are
    /// replugged.
    #[test]
    fn the_order_is_stable_regardless_of_mount_order() {
        let tmp = desktop_sysfs("set-order");
        let forward = "\
40 25 8:17 / /run/media/user/ALPHA rw - vfat /dev/sdb1 rw
41 25 179:1 / /run/media/user/ZULU rw - exfat /dev/mmcblk0p1 rw
";
        let reversed = "\
41 25 179:1 / /run/media/user/ZULU rw - exfat /dev/mmcblk0p1 rw
40 25 8:17 / /run/media/user/ALPHA rw - vfat /dev/sdb1 rw
";
        assert_eq!(
            set_of(forward, &tmp).mount_points(),
            set_of(reversed, &tmp).mount_points()
        );
        assert!(!set_of(forward, &tmp).differs_from(&set_of(reversed, &tmp)));
    }

    /// A device mounted twice (bind mount, or the same stick at two points)
    /// must not produce two identical rows.
    #[test]
    fn the_same_mount_point_is_not_listed_twice() {
        let tmp = desktop_sysfs("set-dup");
        let text = "\
40 25 8:17 / /run/media/user/USB rw - vfat /dev/sdb1 rw
44 25 8:17 / /run/media/user/USB rw - vfat /dev/sdb1 rw
";
        assert_eq!(set_of(text, &tmp).len(), 1);
    }

    /// A LUKS root on device-mapper must not be offered as removable, and an
    /// external drive under `/media` must be.
    #[test]
    fn an_encrypted_laptop_lists_only_its_external_drive() {
        let tmp = TempDir::new("set-luks");
        tmp.block_disk("virtual/block", "dm-0", false);
        tmp.block_disk(SATA_BUS, "sda", false);
        tmp.block_partition(SATA_BUS, "sda", "sda1", 1);
        tmp.block_disk(USB_BUS, "sdc", false);
        tmp.block_partition(USB_BUS, "sdc", "sdc1", 1);
        let set = set_of(fixtures::ENCRYPTED_LAPTOP, &tmp);
        let labels: Vec<&str> = set.volumes().iter().map(super::Volume::label).collect();
        assert_eq!(labels, ["backup"]);
    }

    /// The live scan must not blow up, and on this machine (one internal
    /// `NVMe`, no removable media) it must find nothing.
    #[test]
    fn the_live_scan_lists_no_internal_disks() {
        let set = VolumeSet::scan();
        for volume in set.volumes() {
            assert!(
                volume.is_listable(),
                "{} was scanned but is not listable",
                volume.label()
            );
            assert_ne!(
                volume.mount_point(),
                Path::new("/"),
                "the root filesystem must never appear as removable storage"
            );
        }
    }
}
