//! [`DeviceClass`] -- what kind of storage a mount sits on.
//!
//! Classification decides what the sidebar lists. Getting it wrong is
//! visible in both directions: a missing USB stick makes the feature look
//! broken, and a listed `cgroup2` mount makes it look unfinished.
//!
//! # Why the removable flag is not enough
//!
//! `/sys/class/block/<dev>/removable` is the obvious signal, and it is the
//! *first* one consulted, but it answers a narrower question than it appears
//! to. It reports whether the **medium** can be removed from the drive -- true
//! for optical drives and card readers, false for a USB flash drive, whose
//! medium is soldered to its controller. A USB stick therefore reports `0`,
//! exactly like an internal disk.
//!
//! So a second signal is needed: whether the device hangs off a **hotplug
//! bus** (USB, MMC, IEEE 1394). That is visible in the device's `/sys` link
//! path. Together the two cover the cases a person means by "external drive".
//!
//! # The partition subtlety
//!
//! Filesystems live on partitions (`sdb1`), but the removable flag lives on
//! the parent disk (`sdb`). Reading the flag from the partition finds no such
//! file. The lookup therefore walks to the parent disk first.

use std::path::{Path, PathBuf};

/// What kind of storage backs a mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    /// Storage that can be detached: USB sticks, SD cards, optical discs,
    /// external drives. What the sidebar's Devices section is for.
    Removable,
    /// A fixed disk inside the machine.
    Internal,
    /// Not backed by a block device at all -- `proc`, `sysfs`, `tmpfs`,
    /// `cgroup2`, FUSE mounts of `AppImage` files.
    Virtual,
}

impl DeviceClass {
    /// Where the kernel exposes block devices.
    pub const SYS_BLOCK: &'static str = "/sys/class/block";

    /// Bus names in a device's `/sys` path that mean it can be unplugged.
    ///
    /// `usb` covers flash drives and external enclosures; `mmc` covers SD and
    /// microSD cards; `firewire` covers legacy external drives. Checked as
    /// path components so a disk with `usb` in a *label* is not misread.
    const HOTPLUG_BUSES: [&'static str; 3] = ["usb", "mmc", "firewire"];

    /// Filesystem types that never represent user-facing storage.
    ///
    /// Listed explicitly rather than inferred, because the inference
    /// (`source` does not start with `/dev`) already runs first and this is
    /// the backstop for the cases that slip past it -- notably FUSE mounts,
    /// which do name a source.
    const PSEUDO_FILESYSTEMS: [&'static str; 14] = [
        "proc",
        "sysfs",
        "devtmpfs",
        "devpts",
        "tmpfs",
        "cgroup",
        "cgroup2",
        "securityfs",
        "pstore",
        "efivarfs",
        "bpf",
        "debugfs",
        "tracefs",
        "configfs",
    ];

    /// Whether a filesystem type is a kernel pseudo-filesystem.
    #[must_use]
    pub fn is_pseudo_filesystem(fs_type: &str) -> bool {
        Self::PSEUDO_FILESYSTEMS.contains(&fs_type)
            || fs_type.starts_with("fuse.")
            || fs_type == "autofs"
            || fs_type == "squashfs"
            || fs_type == "overlay"
            || fs_type == "ramfs"
            || fs_type == "mqueue"
            || fs_type == "hugetlbfs"
            || fs_type == "fusectl"
            || fs_type == "binfmt_misc"
    }

    /// Classify a device by name, reading the real `/sys/class/block`.
    ///
    /// `device` is a bare kernel name such as `sdb1` or `nvme0n1p2`.
    #[must_use]
    pub fn of_device(device: &str) -> Self {
        Self::of_device_in(Path::new(Self::SYS_BLOCK), device)
    }

    /// Classify a device against an arbitrary `sysfs` root.
    ///
    /// The root is a parameter so the rules can be tested against hardware
    /// layouts the test machine does not have.
    #[must_use]
    pub fn of_device_in(sys_block: &Path, device: &str) -> Self {
        let node = sys_block.join(device);
        if !node.exists() {
            return Self::Virtual;
        }
        if Self::is_hotplug(&node) || Self::removable_flag(sys_block, device) {
            return Self::Removable;
        }
        Self::Internal
    }

    /// Read the removable flag, following a partition to its parent disk.
    fn removable_flag(sys_block: &Path, device: &str) -> bool {
        let owner = Self::parent_disk(sys_block, device);
        Self::reads_one(&sys_block.join(owner).join("removable"))
    }

    /// The disk a partition belongs to, or the device itself if it is a disk.
    ///
    /// Determined by the presence of a `partition` file rather than by
    /// stripping trailing digits: that string rule breaks on `nvme0n1`, which
    /// is a whole disk whose name ends in a digit.
    fn parent_disk(sys_block: &Path, device: &str) -> String {
        if !sys_block.join(device).join("partition").exists() {
            return device.to_owned();
        }
        // A partition's `/sys/class/block` entry is a symlink into the device
        // tree, where its parent directory is the owning disk.
        std::fs::canonicalize(sys_block.join(device))
            .ok()
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map_or_else(
                || device.to_owned(),
                |name| name.to_string_lossy().into_owned(),
            )
    }

    /// Whether the device sits on a bus whose devices can be unplugged.
    ///
    /// Resolves the symlink into the device tree and looks for a bus name
    /// among its components.
    fn is_hotplug(node: &Path) -> bool {
        let Ok(real) = std::fs::canonicalize(node) else {
            return false;
        };
        Self::path_has_hotplug_bus(&real)
    }

    /// Whether a resolved device path runs through a hotplug bus.
    fn path_has_hotplug_bus(path: &Path) -> bool {
        path.components().any(|component| {
            let text = component.as_os_str().to_string_lossy();
            Self::HOTPLUG_BUSES
                .iter()
                .any(|bus| Self::is_bus_component(&text, bus))
        })
    }

    /// Whether one path component names `bus`.
    ///
    /// The kernel numbers its host controllers and appends a `_host`
    /// suffix, so a USB path reads `usb2` and an SD card path reads
    /// `mmc_host/mmc0` -- neither is an exact match for the bare bus name.
    /// Only those two shapes are accepted, so a volume labelled
    /// `usb-backup` on an internal disk is not misread as removable.
    fn is_bus_component(text: &str, bus: &str) -> bool {
        if text == bus || text == format!("{bus}_host") {
            return true;
        }
        text.strip_prefix(bus)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    }

    /// Whether a `/sys` flag file reads as `1`.
    fn reads_one(path: &PathBuf) -> bool {
        std::fs::read_to_string(path).is_ok_and(|text| text.trim() == "1")
    }

    /// Whether this class should appear in the sidebar's Devices section.
    #[must_use]
    pub const fn is_listable(self) -> bool {
        matches!(self, Self::Removable)
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceClass;
    use crate::test_support::TempDir;
    use std::path::Path;

    #[test]
    fn kernel_pseudo_filesystems_are_recognised() {
        for fs in [
            "proc", "sysfs", "tmpfs", "cgroup2", "devtmpfs", "efivarfs", "tracefs", "bpf",
        ] {
            assert!(DeviceClass::is_pseudo_filesystem(fs), "{fs} is pseudo");
        }
    }

    /// An `AppImage` mounts itself as `fuse.<name>`, and there are three on
    /// this machine right now. They must never appear as storage devices.
    #[test]
    fn fuse_mounts_are_pseudo_whatever_their_suffix() {
        for fs in ["fuse.AppImage", "fuse.portal", "fuse.sshfs"] {
            assert!(DeviceClass::is_pseudo_filesystem(fs), "{fs} is pseudo");
        }
    }

    #[test]
    fn real_filesystems_are_not_pseudo() {
        for fs in ["ext4", "btrfs", "vfat", "exfat", "xfs", "iso9660", "ntfs"] {
            assert!(!DeviceClass::is_pseudo_filesystem(fs), "{fs} is real");
        }
    }

    /// Bus paths taken from real hardware.
    const PCI_SATA: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";
    const PCI_NVME: &str = "pci0000:00/0000:00:1d.0/0000:03:00.0/nvme/nvme0";
    const USB_STICK: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SD_READER: &str = "pci0000:00/0000:00:1c.0/mmc_host/mmc0/mmc0:0001";

    #[test]
    fn a_device_with_no_sys_entry_is_virtual() {
        let tmp = TempDir::new("class-missing");
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "nosuchdev"),
            DeviceClass::Virtual
        );
    }

    #[test]
    fn a_fixed_disk_is_internal() {
        let tmp = TempDir::new("class-internal");
        tmp.block_disk(PCI_SATA, "sda", false);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "sda"),
            DeviceClass::Internal
        );
    }

    /// The flag is what an optical drive reports: its medium really is
    /// separable from the drive.
    #[test]
    fn a_device_with_the_removable_flag_is_removable() {
        let tmp = TempDir::new("class-flag");
        tmp.block_disk(PCI_SATA, "sr0", true);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "sr0"),
            DeviceClass::Removable
        );
    }

    /// The flag lives on the parent disk, not the partition. Reading it from
    /// the partition finds no file and would call a removable disc internal.
    #[test]
    fn a_partition_inherits_its_parent_disks_removable_flag() {
        let tmp = TempDir::new("class-partition");
        tmp.block_disk(PCI_SATA, "sdb", true);
        tmp.block_partition(PCI_SATA, "sdb", "sdb1", 1);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "sdb1"),
            DeviceClass::Removable,
            "the flag on sdb must govern sdb1"
        );
    }

    /// A USB flash drive reports removable=0 -- its medium is soldered to its
    /// controller. Only the bus reveals that it can be unplugged, and the
    /// partition must inherit that too.
    #[test]
    fn a_usb_stick_is_removable_through_its_bus_despite_a_zero_flag() {
        let tmp = TempDir::new("class-usb");
        tmp.block_disk(USB_STICK, "sdc", false);
        tmp.block_partition(USB_STICK, "sdc", "sdc1", 1);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "sdc"),
            DeviceClass::Removable
        );
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "sdc1"),
            DeviceClass::Removable,
            "the partition is as removable as its disk"
        );
    }

    #[test]
    fn an_sd_card_is_removable_through_the_mmc_bus() {
        let tmp = TempDir::new("class-sd");
        tmp.block_disk(SD_READER, "mmcblk0", false);
        tmp.block_partition(SD_READER, "mmcblk0", "mmcblk0p1", 1);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "mmcblk0p1"),
            DeviceClass::Removable
        );
    }

    /// `nvme0n1` is a whole disk whose name ends in a digit. A rule that
    /// found the parent by stripping trailing digits would look for `nvme0n`
    /// and find nothing.
    #[test]
    fn an_nvme_disk_is_not_mistaken_for_a_partition() {
        let tmp = TempDir::new("class-nvme");
        tmp.block_disk(PCI_NVME, "nvme0n1", false);
        tmp.block_partition(PCI_NVME, "nvme0n1", "nvme0n1p2", 2);
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "nvme0n1"),
            DeviceClass::Internal
        );
        assert_eq!(
            DeviceClass::of_device_in(&tmp.class_block(), "nvme0n1p2"),
            DeviceClass::Internal,
            "an internal partition must not be offered as removable storage"
        );
    }

    /// The kernel numbers USB host controllers (`usb2`) and suffixes the MMC
    /// one (`mmc_host`), so neither is a bare match for the bus name.
    #[test]
    fn numbered_and_suffixed_host_controllers_are_recognised() {
        for path in [
            "/sys/devices/pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/block/sdb",
            "/sys/devices/pci0000:00/0000:00:1c.0/mmc_host/mmc0/mmc0:0001/block/mmcblk0",
            "/sys/devices/pci0000:00/0000:00:14.0/usb/1-2/block/sdd",
        ] {
            assert!(
                DeviceClass::path_has_hotplug_bus(Path::new(path)),
                "{path} is hotplug"
            );
        }
    }

    /// An internal `NVMe` or SATA disk runs straight off the PCI bus.
    #[test]
    fn an_internal_disk_path_has_no_hotplug_bus() {
        for path in [
            "/sys/devices/pci0000:00/0000:00:1d.0/0000:03:00.0/nvme/nvme0/nvme0n1",
            "/sys/devices/pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0/block/sda",
        ] {
            assert!(
                !DeviceClass::path_has_hotplug_bus(Path::new(path)),
                "{path} is internal"
            );
        }
    }

    /// A bus name must match a whole path component. A volume labelled
    /// "usb-backup" on an internal disk must not be called removable.
    #[test]
    fn a_bus_name_inside_a_longer_component_does_not_match() {
        assert!(!DeviceClass::path_has_hotplug_bus(Path::new(
            "/sys/devices/pci0000:00/ata1/block/sda/usb-backup-label"
        )));
    }

    #[test]
    fn only_removable_devices_are_listed_in_the_sidebar() {
        assert!(DeviceClass::Removable.is_listable());
        assert!(!DeviceClass::Internal.is_listable());
        assert!(!DeviceClass::Virtual.is_listable());
    }

    /// The disk holding this machine's root filesystem is internal. A live
    /// check that the sysfs rules agree with reality on real hardware.
    #[test]
    fn the_live_root_disk_is_not_classified_removable() {
        let table = crate::MountTable::read();
        let Some(root) = table.at("/") else {
            return;
        };
        let Some(device) = root.device_name() else {
            return;
        };
        assert_ne!(
            DeviceClass::of_device(device),
            DeviceClass::Removable,
            "{device} holds / and cannot be removable"
        );
    }
}
