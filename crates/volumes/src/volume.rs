//! [`Volume`] -- one mounted filesystem as a person thinks of it.
//!
//! A mount entry is a kernel record; a volume is a thing on a desk. The
//! difference is mostly the **name**. `/dev/sdb1` is not what a person calls
//! their USB stick, and `/run/media/user/KINGSTON` is where it happens to be
//! attached, not what it is.
//!
//! Names are resolved in order of how much a human chose them:
//!
//! 1. The **filesystem label**, via `/dev/disk/by-label` -- the name someone
//!    typed when formatting the volume.
//! 2. The **mount point's last component** -- which for `/run/media/user/X`
//!    is usually the label anyway, because that is how automounters name it.
//! 3. The **device name**, as a last resort.

use crate::device_class::DeviceClass;
use crate::mount_entry::MountEntry;
use std::path::{Path, PathBuf};

/// A mounted filesystem, named and classified for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// What to show the user.
    label: String,
    /// Where to navigate when it is chosen.
    mount_point: PathBuf,
    /// The backing device path, e.g. `/dev/sdb1`.
    source: String,
    /// The filesystem type, shown as a detail.
    fs_type: String,
    /// What kind of storage this is.
    class: DeviceClass,
    /// Whether the mount is read-only.
    read_only: bool,
}

impl Volume {
    /// Where automounters place removable media.
    ///
    /// Used only to *name* volumes, never to decide whether something is
    /// removable -- that is the block device's job. A tmpfs mounted under
    /// `/media` is still not a device.
    const MEDIA_ROOTS: [&'static str; 3] = ["/run/media", "/media", "/mnt"];

    /// Where the kernel publishes filesystem labels.
    const BY_LABEL: &'static str = "/dev/disk/by-label";

    /// Build a volume from its parts.
    ///
    /// The mount tables are the only production source, but a volume is a
    /// plain description, and callers that already know what they are
    /// describing -- a downstream test, a future `UDisks2` backend reading
    /// the same facts over D-Bus -- should not have to fabricate a
    /// `mountinfo` line to say so.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        mount_point: impl Into<PathBuf>,
        source: impl Into<String>,
        fs_type: impl Into<String>,
        class: DeviceClass,
        read_only: bool,
    ) -> Self {
        Self {
            label: label.into(),
            mount_point: mount_point.into(),
            source: source.into(),
            fs_type: fs_type.into(),
            class,
            read_only,
        }
    }

    /// Build a volume from a mount entry, reading the real system tables.
    #[must_use]
    pub fn from_mount(entry: &MountEntry) -> Self {
        Self::from_mount_in(
            entry,
            Path::new(DeviceClass::SYS_BLOCK),
            Path::new(Self::BY_LABEL),
        )
    }

    /// Build a volume against arbitrary system tables, for testing.
    #[must_use]
    pub fn from_mount_in(entry: &MountEntry, sys_block: &Path, by_label: &Path) -> Self {
        let class = entry.device_name().map_or(DeviceClass::Virtual, |device| {
            DeviceClass::of_device_in(sys_block, device)
        });
        Self {
            label: Self::name_for(entry, by_label),
            mount_point: PathBuf::from(entry.mount_point()),
            source: entry.source().to_owned(),
            fs_type: entry.fs_type().to_owned(),
            class,
            read_only: entry.read_only(),
        }
    }

    /// Choose the most human name available for a mount.
    fn name_for(entry: &MountEntry, by_label: &Path) -> String {
        Self::filesystem_label(entry, by_label)
            .or_else(|| Self::mount_point_name(entry))
            .or_else(|| entry.device_name().map(str::to_owned))
            .unwrap_or_else(|| entry.mount_point().to_owned())
    }

    /// The filesystem label, by finding which `by-label` link points at this
    /// device.
    ///
    /// The directory is scanned rather than reverse-engineering a filename,
    /// because the label is the link *name* and the device is its target --
    /// there is no way to go from device to label except by looking.
    fn filesystem_label(entry: &MountEntry, by_label: &Path) -> Option<String> {
        let source = std::fs::canonicalize(entry.source()).ok()?;
        let links = std::fs::read_dir(by_label).ok()?;
        for link in links.flatten() {
            if std::fs::canonicalize(link.path()).ok()? == source {
                return Some(link.file_name().to_string_lossy().into_owned());
            }
        }
        None
    }

    /// The last component of the mount point, when it is somewhere a person
    /// would recognise.
    ///
    /// Restricted to the automounter roots: the last component of `/home` is
    /// "home", which is a fine name, but the last component of
    /// `/var/lib/docker/overlay2/abc123/merged` is not.
    fn mount_point_name(entry: &MountEntry) -> Option<String> {
        let point = entry.mount_point();
        let under_media = Self::MEDIA_ROOTS
            .iter()
            .any(|root| point.starts_with(&format!("{root}/")));
        if !under_media {
            return None;
        }
        Path::new(point)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }

    /// The name to show.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Where choosing this volume navigates to.
    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    /// The backing device path.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The filesystem type.
    #[must_use]
    pub fn fs_type(&self) -> &str {
        &self.fs_type
    }

    /// What kind of storage this is.
    #[must_use]
    pub const fn class(&self) -> DeviceClass {
        self.class
    }

    /// Whether the volume is mounted read-only.
    ///
    /// Worth showing: it is the difference between "I cannot write here" and
    /// "something is broken", and a browser that silently fails to paste into
    /// a read-only volume looks like the latter.
    #[must_use]
    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    /// Whether this volume belongs in the sidebar's Devices section.
    #[must_use]
    pub const fn is_listable(&self) -> bool {
        self.class.is_listable()
    }

    /// A short description for the row's second line, e.g. `vfat, read-only`.
    #[must_use]
    pub fn detail(&self) -> String {
        if self.read_only {
            format!("{}, read-only", self.fs_type)
        } else {
            self.fs_type.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Volume;
    use crate::device_class::DeviceClass;
    use crate::mount_entry::MountEntry;
    use crate::test_support::TempDir;
    use std::path::Path;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";

    /// Parse a line and build a volume against a fake sysfs.
    fn volume(line: &str, tmp: &TempDir) -> Volume {
        let entry = MountEntry::parse(line).expect("a well-formed line");
        Volume::from_mount_in(&entry, &tmp.class_block(), &tmp.path().join("by-label"))
    }

    #[test]
    fn a_usb_stick_is_named_from_its_mount_point() {
        let tmp = TempDir::new("vol-usb");
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        let vol = volume(
            "40 25 8:17 / /run/media/user/KINGSTON rw,relatime - vfat /dev/sdb1 rw",
            &tmp,
        );
        assert_eq!(vol.label(), "KINGSTON");
        assert_eq!(vol.mount_point(), Path::new("/run/media/user/KINGSTON"));
        assert_eq!(vol.class(), DeviceClass::Removable);
        assert!(vol.is_listable());
    }

    /// Automounters also use `/media` and `/mnt`.
    #[test]
    fn every_automount_root_yields_a_name() {
        let tmp = TempDir::new("vol-roots");
        tmp.block_disk(USB_BUS, "sdc", false);
        for (point, expected) in [
            ("/run/media/user/A", "A"),
            ("/media/backup", "backup"),
            ("/mnt/usb", "usb"),
        ] {
            let line = format!("40 25 8:33 / {point} rw - ext4 /dev/sdc rw");
            assert_eq!(volume(&line, &tmp).label(), expected);
        }
    }

    /// A deep internal mount point must not be named from its last
    /// component, which is meaningless out of context.
    #[test]
    fn a_deep_system_path_falls_back_to_the_device_name() {
        let tmp = TempDir::new("vol-deep");
        tmp.block_disk("pci0000:00/ata1", "sda", false);
        let vol = volume(
            "40 25 8:1 / /var/lib/docker/overlay2/abc123/merged rw - ext4 /dev/sda rw",
            &tmp,
        );
        assert_eq!(vol.label(), "sda", "'merged' would tell the user nothing");
    }

    #[test]
    fn a_read_only_volume_says_so_in_its_detail() {
        let tmp = TempDir::new("vol-ro");
        tmp.block_disk(USB_BUS, "sr0", true);
        let vol = volume(
            "42 25 11:0 / /run/media/user/AUDIO ro,relatime - iso9660 /dev/sr0 ro",
            &tmp,
        );
        assert!(vol.read_only());
        assert_eq!(vol.detail(), "iso9660, read-only");
    }

    #[test]
    fn a_writable_volume_shows_only_its_filesystem() {
        let tmp = TempDir::new("vol-rw");
        tmp.block_disk(USB_BUS, "sdb", false);
        let vol = volume("40 25 8:17 / /media/stick rw - vfat /dev/sdb rw", &tmp);
        assert!(!vol.read_only());
        assert_eq!(vol.detail(), "vfat");
    }

    /// A pseudo-filesystem has no block device, so it cannot be a volume the
    /// sidebar offers.
    #[test]
    fn a_pseudo_filesystem_is_virtual_and_not_listable() {
        let tmp = TempDir::new("vol-pseudo");
        let vol = volume("22 25 0:21 / /sys rw - sysfs sysfs rw", &tmp);
        assert_eq!(vol.class(), DeviceClass::Virtual);
        assert!(!vol.is_listable());
    }

    /// An internal disk is a real device but is not offered as removable
    /// storage.
    #[test]
    fn an_internal_disk_is_not_listable() {
        let tmp = TempDir::new("vol-internal");
        tmp.block_disk("pci0000:00/0000:00:1d.0/nvme/nvme0", "nvme0n1", false);
        tmp.block_partition(
            "pci0000:00/0000:00:1d.0/nvme/nvme0",
            "nvme0n1",
            "nvme0n1p2",
            2,
        );
        let vol = volume("25 1 0:33 /@ / rw - btrfs /dev/nvme0n1p2 rw", &tmp);
        assert_eq!(vol.class(), DeviceClass::Internal);
        assert!(!vol.is_listable());
    }

    /// A mount point with a space arrives octal-escaped; the label must be
    /// the decoded text, or the sidebar shows `My\040Drive`.
    #[test]
    fn a_label_with_a_space_is_decoded() {
        let tmp = TempDir::new("vol-space");
        tmp.block_disk(USB_BUS, "sdb", false);
        let vol = volume(
            "40 25 8:17 / /run/media/user/My\\040Drive rw - vfat /dev/sdb rw",
            &tmp,
        );
        assert_eq!(vol.label(), "My Drive");
        assert_eq!(vol.mount_point(), Path::new("/run/media/user/My Drive"));
    }
}
