//! [`AttachedDevice`] -- a removable volume that is plugged in, mounted or not.
//!
//! [`Volume`] answers "where can I browse?", and can only exist for storage
//! that is already attached, because it is built from a mount table entry. A
//! stick that is plugged in but not mounted has no entry, so it cannot be
//! represented at all -- and therefore cannot be listed, or offered a Mount
//! action.
//!
//! This type covers both states. It is assembled from three sources, each
//! answering what the others cannot:
//!
//! | source | contributes |
//! |---|---|
//! | `/sys/class/block` | that the device exists, its size, that it is removable |
//! | `/run/udev/data` | the filesystem type and label, read from the superblock |
//! | `/proc/self/mountinfo` | whether it is mounted, and where |
//!
//! # Joining them by number, not by name
//!
//! The three tables are tied together by the **major:minor device numbers**,
//! never by the device name. A name match seems to work and then fails on
//! exactly the interesting cases: device-mapper reports `/dev/mapper/cryptroot`
//! in the mount table while `/sys` calls it `dm-0`, and a `by-id` or `by-uuid`
//! path in the mount table names no `/sys` node at all. The numbers are the
//! kernel's own identity for a device and always agree.
//!
//! [`Volume`]: crate::volume::Volume

use crate::block_device::BlockDevice;
use crate::udev_properties::UdevProperties;
use std::path::{Path, PathBuf};

/// A removable device that is present, with its filesystem and mount state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedDevice {
    /// The `/dev` path, e.g. `/dev/sdb1`.
    source: String,
    /// The kernel device name, e.g. `sdb1`.
    name: String,
    /// The filesystem label, when the volume carries one.
    label: Option<String>,
    /// The filesystem type, e.g. `vfat`.
    fs_type: String,
    /// Size in bytes.
    bytes: u64,
    /// Where it is mounted, when it is.
    mounted_at: Option<PathBuf>,
    /// Major device number.
    major: u32,
    /// Minor device number.
    minor: u32,
}

impl AttachedDevice {
    /// Describe a device from its `/sys` node and udev's properties.
    ///
    /// `None` when the device holds nothing mountable: an empty card slot, an
    /// unformatted stick, swap, or a whole disk carrying only a partition
    /// table. Each of those is present and removable, and mounting any of
    /// them can only fail.
    #[must_use]
    pub fn describe(device: &BlockDevice, properties: &UdevProperties) -> Option<Self> {
        if !device.is_candidate() || !properties.is_mountable_filesystem() {
            return None;
        }
        Some(Self {
            source: device.device_path(),
            name: device.name().to_owned(),
            label: properties.fs_label().map(str::to_owned),
            fs_type: properties.fs_type()?.to_owned(),
            bytes: device.bytes(),
            mounted_at: None,
            major: device.major(),
            minor: device.minor(),
        })
    }

    /// Record where this device is mounted.
    #[must_use]
    pub fn mounted_at(mut self, point: impl Into<PathBuf>) -> Self {
        self.mounted_at = Some(point.into());
        self
    }

    /// The `/dev` path.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The kernel device name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The filesystem label, when there is one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The filesystem type.
    #[must_use]
    pub fn fs_type(&self) -> &str {
        &self.fs_type
    }

    /// Size in bytes.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
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

    /// Where the device is mounted, if it is.
    #[must_use]
    pub fn mount_point(&self) -> Option<&Path> {
        self.mounted_at.as_deref()
    }

    /// Whether the device is mounted.
    #[must_use]
    pub const fn is_mounted(&self) -> bool {
        self.mounted_at.is_some()
    }

    /// Whether a Mount action applies to this device.
    #[must_use]
    pub const fn is_mountable(&self) -> bool {
        !self.is_mounted()
    }

    /// The name to show in the sidebar.
    ///
    /// The label when the volume has one, and the device name otherwise. An
    /// unlabelled stick is common, and showing an empty row would be worse
    /// than showing `sdb1`.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::AttachedDevice;
    use crate::block_device::BlockDevice;
    use crate::test_support::{fixtures, TempDir};
    use crate::udev_properties::UdevProperties;
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SATA_BUS: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";

    /// A removable stick node with the given size.
    fn stick(tmp: &TempDir, sectors: u64) -> BlockDevice {
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, sectors);
        BlockDevice::read(&tmp.class_block(), "sdb1").expect("the partition")
    }

    #[test]
    fn an_unmounted_stick_is_described_and_offers_mounting() {
        let tmp = TempDir::new("attached-unmounted");
        let device = stick(&tmp, 1_953_792);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props).expect("a mountable stick");
        assert_eq!(attached.source(), "/dev/sdb1");
        assert_eq!(attached.label(), Some("EFI"));
        assert_eq!(attached.fs_type(), "vfat");
        assert_eq!(attached.bytes(), 1_953_792 * 512);
        assert!(!attached.is_mounted());
        assert!(attached.is_mountable(), "this is the whole point");
    }

    /// The state that could not be represented before: mounted devices keep
    /// working, and stop offering a Mount action.
    #[test]
    fn a_mounted_device_reports_its_point_and_stops_offering_mounting() {
        let tmp = TempDir::new("attached-mounted");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props)
            .expect("mountable")
            .mounted_at("/run/media/user/EFI");
        assert!(attached.is_mounted());
        assert_eq!(
            attached.mount_point(),
            Some(Path::new("/run/media/user/EFI"))
        );
        assert!(!attached.is_mountable());
    }

    /// Swap is removable, present, and has a filesystem type. Offering it
    /// would produce a privileged mount attempt that can only fail.
    #[test]
    fn swap_is_not_described_as_a_device_to_mount() {
        let tmp = TempDir::new("attached-swap");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_SWAP);
        assert!(AttachedDevice::describe(&device, &props).is_none());
    }

    /// An unformatted stick is a real removable device that holds no
    /// filesystem at all.
    #[test]
    fn an_unformatted_stick_is_not_described() {
        let tmp = TempDir::new("attached-blank");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse("E:ID_PATH=pci-0000:00:14.0-usb-0:1\n");
        assert!(AttachedDevice::describe(&device, &props).is_none());
    }

    /// udev populates asynchronously, so a device plugged in during a scan
    /// may have no record yet. It must be skipped, not guessed at.
    #[test]
    fn a_device_udev_has_not_processed_is_not_described() {
        let tmp = TempDir::new("attached-unknown");
        let device = stick(&tmp, 1_024);
        assert!(AttachedDevice::describe(&device, &UdevProperties::default()).is_none());
    }

    #[test]
    fn an_empty_card_slot_is_not_described() {
        let tmp = TempDir::new("attached-slot");
        let device = stick(&tmp, 0);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        assert!(AttachedDevice::describe(&device, &props).is_none());
    }

    /// An internal disk holds a perfectly good filesystem and must still
    /// never be offered as removable storage.
    #[test]
    fn an_internal_disk_is_not_described() {
        let tmp = TempDir::new("attached-internal");
        tmp.block_disk(SATA_BUS, "sda", false);
        tmp.block_partition(SATA_BUS, "sda", "sda1", 1);
        tmp.sized(SATA_BUS, "sda", Some("sda1"), 8, 1, 1_024);
        let device = BlockDevice::read(&tmp.class_block(), "sda1").expect("the partition");
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        assert!(AttachedDevice::describe(&device, &props).is_none());
    }

    /// An unlabelled stick is common. An empty sidebar row would be worse
    /// than showing the kernel name.
    #[test]
    fn an_unlabelled_device_shows_its_kernel_name() {
        let tmp = TempDir::new("attached-nolabel");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse("E:ID_FS_TYPE=exfat\nE:ID_FS_USAGE=filesystem\n");
        let attached = AttachedDevice::describe(&device, &props).expect("mountable");
        assert_eq!(attached.label(), None);
        assert_eq!(attached.display_name(), "sdb1");
    }

    #[test]
    fn a_labelled_device_shows_its_label() {
        let tmp = TempDir::new("attached-label");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props).expect("mountable");
        assert_eq!(attached.display_name(), "EFI");
    }

    /// The numbers are how this device is matched against the mount table,
    /// so they must survive being carried through.
    #[test]
    fn the_device_numbers_are_carried_for_matching() {
        let tmp = TempDir::new("attached-numbers");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props).expect("mountable");
        assert_eq!((attached.major(), attached.minor()), (8, 17));
    }
}
