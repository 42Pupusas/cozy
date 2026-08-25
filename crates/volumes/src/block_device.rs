//! [`BlockDevice`] -- one node of `/sys/class/block`.
//!
//! The mount table can only describe storage that is **already attached**. To
//! offer a stick that is plugged in but not mounted, the devices themselves
//! have to be enumerated, and `/sys/class/block` is the kernel's own list of
//! them.
//!
//! Each entry gives the facts that decide whether a device is worth offering:
//!
//! ```text
//! /sys/class/block/sdb1/dev          "8:17"    the device numbers
//! /sys/class/block/sdb1/size         "1953792" size in 512-byte SECTORS
//! /sys/class/block/sdb1/partition    "1"       present only on partitions
//! ```
//!
//! # Two traps in these files
//!
//! `size` is in **512-byte sectors regardless of the device's real sector
//! size**, so a 4K-sector disk reports the same unit as any other. Treating
//! the number as bytes understates a volume by a factor of 512.
//!
//! A size of **zero** is the normal report from an empty card-reader slot or
//! an optical drive with no disc. Such a device is present, removable, and
//! completely uninteresting: offering to mount it produces a guaranteed
//! failure, so zero size is treated as "nothing there".

use crate::device_class::DeviceClass;
use std::path::Path;

/// One block device as the kernel lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDevice {
    /// The kernel name, e.g. `sdb1`.
    name: String,
    /// Major device number.
    major: u32,
    /// Minor device number.
    minor: u32,
    /// Size in 512-byte sectors, as `/sys` reports it.
    sectors: u64,
    /// Whether this node is a partition of some disk.
    partition: bool,
    /// What kind of storage this is.
    class: DeviceClass,
}

impl BlockDevice {
    /// The size unit `/sys/class/block/*/size` uses, in bytes.
    ///
    /// Fixed at 512 by the kernel interface, whatever the hardware's real
    /// sector size.
    pub const SECTOR_BYTES: u64 = 512;

    /// Read one device by name from a `sysfs` block directory.
    ///
    /// `None` when the node does not exist or has no `dev` file, which is
    /// the case for the directory entries that are not devices.
    #[must_use]
    pub fn read(sys_block: &Path, name: &str) -> Option<Self> {
        let node = sys_block.join(name);
        let (major, minor) = Self::device_numbers(&node)?;
        Some(Self {
            name: name.to_owned(),
            major,
            minor,
            sectors: Self::read_number(&node.join("size")).unwrap_or(0),
            partition: node.join("partition").exists(),
            class: DeviceClass::of_device_in(sys_block, name),
        })
    }

    /// Parse the `dev` file, which holds `major:minor`.
    fn device_numbers(node: &Path) -> Option<(u32, u32)> {
        let text = std::fs::read_to_string(node.join("dev")).ok()?;
        let (major, minor) = text.trim().split_once(':')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    /// Read a `/sys` file holding a single number.
    fn read_number(path: &Path) -> Option<u64> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// The kernel name, e.g. `sdb1`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The `/dev` path of this device.
    #[must_use]
    pub fn device_path(&self) -> String {
        format!("/dev/{}", self.name)
    }

    /// Major device number.
    #[must_use]
    pub const fn major(&self) -> u32 {
        self.major
    }

    /// Minor device number.
    #[must_use]
    pub const fn minor(&self) -> u32 {
        self.minor
    }

    /// Size in 512-byte sectors.
    #[must_use]
    pub const fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Size in bytes.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.sectors.saturating_mul(Self::SECTOR_BYTES)
    }

    /// Whether this node is a partition rather than a whole disk.
    #[must_use]
    pub const fn is_partition(&self) -> bool {
        self.partition
    }

    /// What kind of storage this is.
    #[must_use]
    pub const fn class(&self) -> DeviceClass {
        self.class
    }

    /// Whether the device has no medium in it.
    ///
    /// An empty card slot and an optical drive with no disc both appear in
    /// `/sys` and both report size zero.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sectors == 0
    }

    /// Whether this device could hold a filesystem a person wants mounted.
    ///
    /// Removable, and not an empty drive. Whether it *does* hold one is a
    /// question for udev, which has read the superblock.
    #[must_use]
    pub const fn is_candidate(&self) -> bool {
        self.class.is_listable() && !self.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::BlockDevice;
    use crate::device_class::DeviceClass;
    use crate::test_support::TempDir;
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SATA_BUS: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";

    #[test]
    fn a_partition_reports_its_numbers_size_and_class() {
        let tmp = TempDir::new("block-partition");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        let device = BlockDevice::read(&tmp.class_block(), "sdb1").expect("the partition");
        assert_eq!(device.name(), "sdb1");
        assert_eq!(device.device_path(), "/dev/sdb1");
        assert_eq!((device.major(), device.minor()), (8, 17));
        assert!(device.is_partition());
        assert_eq!(device.class(), DeviceClass::Removable);
        assert!(device.is_candidate());
    }

    /// `size` counts 512-byte sectors whatever the hardware's real sector
    /// size. Reading it as bytes understates the volume 512-fold.
    #[test]
    fn size_is_counted_in_512_byte_sectors() {
        let tmp = TempDir::new("block-size");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.sized(USB_BUS, "sdb", None, 8, 16, 2_048);
        let device = BlockDevice::read(&tmp.class_block(), "sdb").expect("the disk");
        assert_eq!(device.sectors(), 2_048);
        assert_eq!(device.bytes(), 1_048_576, "2048 sectors is one mebibyte");
    }

    /// An empty card-reader slot is present and removable but holds nothing.
    /// Offering to mount it can only fail.
    #[test]
    fn an_empty_card_slot_is_not_a_candidate() {
        let tmp = TempDir::new("block-empty-slot");
        tmp.block_disk("pci0000:00/0000:00:1c.0/mmc_host/mmc0", "mmcblk0", true);
        tmp.sized("pci0000:00/0000:00:1c.0/mmc_host/mmc0", "mmcblk0", None, 179, 0, 0);
        let device = BlockDevice::read(&tmp.class_block(), "mmcblk0").expect("the slot");
        assert!(device.is_empty());
        assert!(!device.is_candidate(), "an empty slot must not be offered");
    }

    #[test]
    fn an_internal_disk_is_not_a_candidate() {
        let tmp = TempDir::new("block-internal");
        tmp.block_disk(SATA_BUS, "sda", false);
        tmp.sized(SATA_BUS, "sda", None, 8, 0, 1_000_000);
        let device = BlockDevice::read(&tmp.class_block(), "sda").expect("the disk");
        assert_eq!(device.class(), DeviceClass::Internal);
        assert!(!device.is_candidate());
    }

    #[test]
    fn a_whole_disk_is_not_a_partition() {
        let tmp = TempDir::new("block-disk");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.sized(USB_BUS, "sdb", None, 8, 16, 100);
        let device = BlockDevice::read(&tmp.class_block(), "sdb").expect("the disk");
        assert!(!device.is_partition());
    }

    #[test]
    fn a_node_that_does_not_exist_reads_as_nothing() {
        let tmp = TempDir::new("block-absent");
        assert!(BlockDevice::read(&tmp.class_block(), "sdz9").is_none());
    }

    /// A `dev` file the kernel would never write must not panic or produce a
    /// device with nonsense numbers.
    #[test]
    fn a_malformed_dev_file_reads_as_nothing() {
        let tmp = TempDir::new("block-malformed");
        for (name, text) in [("bad1", "notanumber"), ("bad2", "8"), ("bad3", "")] {
            tmp.write(&format!("class/block/{name}/dev"), text);
            assert!(
                BlockDevice::read(&tmp.class_block(), name).is_none(),
                "{name} must be rejected"
            );
        }
    }

    /// A device with no `size` file is readable, but must not be offered:
    /// an unknown size cannot be assumed to be a full one.
    #[test]
    fn a_device_without_a_size_file_reads_as_empty() {
        let tmp = TempDir::new("block-nosize");
        tmp.write("class/block/loop0/dev", "7:0\n");
        let device = BlockDevice::read(&tmp.class_block(), "loop0").expect("the node");
        assert!(device.is_empty());
        assert!(!device.is_candidate());
    }

    /// The live check: every device this machine has must read cleanly, and
    /// its numbers must match what the node's own `dev` file says.
    #[test]
    fn every_live_block_device_reads_consistently() {
        let sys_block = Path::new(DeviceClass::SYS_BLOCK);
        let Ok(entries) = std::fs::read_dir(sys_block) else {
            return;
        };
        let mut seen = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(device) = BlockDevice::read(sys_block, &name) else {
                continue;
            };
            let text = std::fs::read_to_string(sys_block.join(&name).join("dev")).expect("dev");
            assert_eq!(
                text.trim(),
                format!("{}:{}", device.major(), device.minor()),
                "{name} numbers must round-trip"
            );
            seen += 1;
        }
        assert!(seen > 0, "this machine has block devices");
    }
}
