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
use crate::mount_point::MountPoint;
use crate::mount_request::MountRequest;
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

    /// Describe a device directly, without reading `/sys`.
    ///
    /// The counterpart to [`Self::describe`], which is the scanning path.
    /// Public for the same reason [`crate::ScriptedBackend`] is: the
    /// interesting tests for mounting live in the file browser, and they
    /// cannot conjure a `/sys` tree and a udev record for a stick that is not
    /// plugged in. [`crate::Volume::new`] is public for exactly this reason
    /// already.
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        label: Option<&str>,
        fs_type: impl Into<String>,
        bytes: u64,
        major: u32,
        minor: u32,
    ) -> Self {
        let source = source.into();
        let name = Path::new(&source)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&source)
            .to_owned();
        Self {
            source,
            name,
            label: label.map(str::to_owned),
            fs_type: fs_type.into(),
            bytes,
            mounted_at: None,
            major,
            minor,
        }
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

    /// Describe the mount this device is asking for.
    ///
    /// `None` when the device is already mounted, or when the label yields no
    /// usable path component -- see [`MountPoint`] for why a label may not be
    /// turned into a path by concatenation.
    ///
    /// Built here rather than at the call site so that the destination is
    /// derived from the label in exactly one place. A caller assembling its
    /// own path would be re-deriving the sanitising, and getting it wrong
    /// there means a privileged mounter attaching a filesystem over `/etc`.
    #[must_use]
    pub fn mount_request(&self, user: &str, uid: u32, gid: u32) -> Option<MountRequest> {
        if self.is_mounted() {
            return None;
        }
        let point = MountPoint::for_user(user, self.label(), &self.source)?;
        Some(MountRequest::new(
            self.source.clone(),
            self.fs_type.clone(),
            point,
            uid,
            gid,
        ))
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
    use crate::test_support::{TempDir, fixtures};
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

    #[test]
    fn a_mountable_device_describes_the_mount_it_wants() {
        let tmp = TempDir::new("attached-request");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props).expect("mountable");
        let request = attached
            .mount_request("ada", 1000, 1000)
            .expect("a request");
        assert_eq!(request.device(), "/dev/sdb1");
        assert_eq!(request.fs_type(), "vfat");
        assert_eq!(
            request.mount_point().path(),
            Path::new("/run/media/ada/EFI")
        );
    }

    /// A label is chosen by whoever formatted the stick. Building the
    /// destination by concatenation would let it escape the media root and
    /// ask a privileged mounter to attach over a system directory.
    #[test]
    fn a_traversing_label_cannot_escape_the_media_root() {
        let tmp = TempDir::new("attached-evil");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(
            "E:ID_FS_TYPE=vfat\nE:ID_FS_USAGE=filesystem\nE:ID_FS_LABEL=..\\x2f..\\x2fetc\n",
        );
        let attached = AttachedDevice::describe(&device, &props).expect("mountable");
        let request = attached
            .mount_request("ada", 1000, 1000)
            .expect("a request");
        let path = request.mount_point().path();
        assert_eq!(
            path.parent(),
            Some(Path::new("/run/media/ada")),
            "the mount point must stay directly under the user's media root"
        );
        assert!(!path.to_string_lossy().contains("/etc"));
    }

    /// An already-mounted device has nothing to ask for, and re-mounting it
    /// would attach a second copy over the first.
    #[test]
    fn a_mounted_device_asks_for_no_mount() {
        let tmp = TempDir::new("attached-already");
        let device = stick(&tmp, 1_024);
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        let attached = AttachedDevice::describe(&device, &props)
            .expect("mountable")
            .mounted_at("/run/media/ada/EFI");
        assert!(attached.mount_request("ada", 1000, 1000).is_none());
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
