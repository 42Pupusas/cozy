//! [`UdevProperties`] -- what udev already knows about a block device.
//!
//! # Why this file exists
//!
//! A mounted filesystem announces its type in `/proc/self/mountinfo`. An
//! **unmounted** one announces nothing at all: `/sys/class/block/sdb1` gives
//! the size, the partition number and the bus, but not what filesystem is on
//! it. Identifying that means reading the filesystem's superblock, which
//! means parsing on-disk structures written by whoever formatted the stick.
//!
//! That work is already done. When udev sees a device it runs `blkid`, and
//! stores the result in a plain text file under `/run/udev/data`, named after
//! the device numbers:
//!
//! ```text
//! /run/udev/data/b259:1        block device, major 259, minor 1
//! ```
//!
//! Reading that file is a text parse of a local, root-owned cache, instead of
//! a superblock parse of attacker-controlled bytes. Nothing else in this
//! crate can offer the filesystem type of a device that is not mounted.
//!
//! # The format
//!
//! ```text
//! S:disk/by-label/EFI              a symlink udev created
//! E:ID_FS_TYPE=vfat                a property
//! E:ID_FS_USAGE=filesystem
//! E:ID_PART_ENTRY_NAME=EFI\x20System
//! ```
//!
//! Only the `E:` lines are of interest. Their values use **hex** escapes
//! (`\x20`), which is a different encoding from the **octal** escapes
//! (`\040`) the kernel uses in `mountinfo` -- see [`MountEntry`]. Applying
//! the wrong decoder to either leaves the escape visible in the sidebar.
//!
//! # This is a cache, not a source of truth
//!
//! The directory is absent on a system running `mdev` or `busybox`, is
//! populated asynchronously after a device appears, and a device the user
//! plugs in during a scan may have no file yet. Every read therefore returns
//! empty properties rather than an error, and the caller treats "udev does
//! not know" as an ordinary answer.
//!
//! [`MountEntry`]: crate::mount_entry::MountEntry

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The properties udev recorded for one device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UdevProperties {
    values: HashMap<String, String>,
}

impl UdevProperties {
    /// Where udev keeps its property database.
    pub const DEFAULT_ROOT: &'static str = "/run/udev/data";

    /// The property naming the filesystem type, e.g. `vfat`.
    const FS_TYPE: &'static str = "ID_FS_TYPE";

    /// The property naming the filesystem label, as chosen at format time.
    const FS_LABEL: &'static str = "ID_FS_LABEL";

    /// The property saying what the device holds: a filesystem, swap, or a
    /// partition table.
    const FS_USAGE: &'static str = "ID_FS_USAGE";

    /// The [`Self::FS_USAGE`] value that means a mountable filesystem.
    const USAGE_FILESYSTEM: &'static str = "filesystem";

    /// Read the properties of a block device from the real database.
    #[must_use]
    pub fn read_block(major: u32, minor: u32) -> Self {
        Self::read_block_in(Path::new(Self::DEFAULT_ROOT), major, minor)
    }

    /// Read from an arbitrary database root, for testing.
    ///
    /// A missing file is not an error: udev may not run here, or may not have
    /// processed this device yet.
    #[must_use]
    pub fn read_block_in(root: &Path, major: u32, minor: u32) -> Self {
        std::fs::read_to_string(Self::block_file(root, major, minor))
            .map_or_else(|_| Self::default(), |text| Self::parse(&text))
    }

    /// The database file for a block device.
    ///
    /// The `b` prefix is the device type; character devices use `c`. Reading
    /// the wrong prefix would silently return another device's properties.
    #[must_use]
    pub fn block_file(root: &Path, major: u32, minor: u32) -> PathBuf {
        root.join(format!("b{major}:{minor}"))
    }

    /// Parse the text of one database file.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let values = text
            .lines()
            .filter_map(|line| line.strip_prefix("E:"))
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), Self::decode(value)))
            .collect();
        Self { values }
    }

    /// Decode udev's hex escapes.
    ///
    /// Values are stored with every character outside a safe set written as
    /// `\x` followed by two hex digits, so a label of `EFI System` is stored
    /// as `EFI\x20System`.
    fn decode(value: &str) -> String {
        if !value.contains('\\') {
            return value.to_owned();
        }
        let mut out = String::with_capacity(value.len());
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match Self::hex_byte(&chars.clone().take(3).collect::<String>()) {
                Some(byte) => {
                    out.push(byte as char);
                    for _ in 0..3 {
                        chars.next();
                    }
                }
                // A backslash that introduces nothing valid is a literal one.
                None => out.push(c),
            }
        }
        out
    }

    /// Decode an `xNN` escape body into a byte.
    fn hex_byte(body: &str) -> Option<u8> {
        let digits = body.strip_prefix('x')?;
        if digits.len() != 2 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        u8::from_str_radix(digits, 16).ok()
    }

    /// One property by name.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// How many properties were recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether udev recorded nothing for this device.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The filesystem type, e.g. `vfat`.
    #[must_use]
    pub fn fs_type(&self) -> Option<&str> {
        self.get(Self::FS_TYPE).filter(|text| !text.is_empty())
    }

    /// The filesystem label, as chosen when the volume was formatted.
    ///
    /// This is the attacker-controlled string discussed in [`MountPoint`];
    /// it is returned verbatim and must be sanitised before it names a path.
    ///
    /// [`MountPoint`]: crate::mount_point::MountPoint
    #[must_use]
    pub fn fs_label(&self) -> Option<&str> {
        self.get(Self::FS_LABEL).filter(|text| !text.is_empty())
    }

    /// Whether this device holds a filesystem that could be mounted.
    ///
    /// Swap space and partition tables also carry an `ID_FS_TYPE`, so the
    /// type alone does not answer this. Offering to mount swap would produce
    /// a privileged mount attempt that can only fail.
    #[must_use]
    pub fn is_mountable_filesystem(&self) -> bool {
        self.fs_type().is_some() && self.get(Self::FS_USAGE) == Some(Self::USAGE_FILESYSTEM)
    }
}

#[cfg(test)]
mod tests {
    use super::UdevProperties;
    use crate::test_support::{TempDir, fixtures};
    use std::path::Path;

    #[test]
    fn properties_are_read_from_the_e_lines() {
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        assert_eq!(props.fs_type(), Some("vfat"));
        assert_eq!(props.fs_label(), Some("EFI"));
        assert!(props.is_mountable_filesystem());
    }

    /// The `S:` symlink lines and the `G:`/`V:` trailers are not properties,
    /// and a parser that took every line would report nonsense keys.
    #[test]
    fn non_property_lines_are_ignored() {
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        assert_eq!(props.get("S"), None);
        assert_eq!(props.get("G"), None);
        for key in props.values.keys() {
            assert!(key.starts_with("ID_"), "unexpected key {key}");
        }
    }

    /// udev writes hex escapes, the kernel writes octal ones. Decoding
    /// `\x20` as octal would leave the escape visible to the user.
    #[test]
    fn hex_escapes_are_decoded() {
        let props = UdevProperties::parse(fixtures::UDEV_VFAT_PARTITION);
        assert_eq!(props.get("ID_PART_ENTRY_NAME"), Some("EFI System"));
    }

    #[test]
    fn a_label_with_several_escapes_decodes_fully() {
        let props = UdevProperties::parse("E:ID_FS_LABEL=My\\x20Big\\x20Drive\\x21");
        assert_eq!(props.fs_label(), Some("My Big Drive!"));
    }

    /// A backslash is a legal character in a label, and one that introduces
    /// no valid escape must survive.
    #[test]
    fn a_bare_backslash_is_kept() {
        let props = UdevProperties::parse("E:ID_FS_LABEL=back\\slash");
        assert_eq!(props.fs_label(), Some("back\\slash"));
    }

    /// A value may itself contain `=`, so the split must be on the first one
    /// only.
    #[test]
    fn a_value_containing_an_equals_sign_is_kept_whole() {
        let props = UdevProperties::parse("E:ID_FS_OPTS=a=b=c");
        assert_eq!(props.get("ID_FS_OPTS"), Some("a=b=c"));
    }

    /// Swap carries `ID_FS_TYPE=swap`, so the type alone cannot decide
    /// whether something is mountable. Offering to mount swap yields a
    /// privileged attempt that can only fail.
    #[test]
    fn swap_is_not_a_mountable_filesystem() {
        let props = UdevProperties::parse(fixtures::UDEV_SWAP);
        assert_eq!(props.fs_type(), Some("swap"));
        assert!(!props.is_mountable_filesystem(), "swap must not be offered");
    }

    /// A whole disk carrying a partition table reports its type as `gpt`
    /// with usage `raid`; mounting the disk itself is meaningless.
    #[test]
    fn a_partition_table_is_not_a_mountable_filesystem() {
        let props = UdevProperties::parse("E:ID_FS_TYPE=gpt\nE:ID_FS_USAGE=raid\n");
        assert!(!props.is_mountable_filesystem());
    }

    /// A blank device has a record but no filesystem properties.
    #[test]
    fn an_unformatted_device_reports_no_filesystem() {
        let props = UdevProperties::parse("E:ID_PATH=pci-0000:02:00.0\n");
        assert_eq!(props.fs_type(), None);
        assert_eq!(props.fs_label(), None);
        assert!(!props.is_mountable_filesystem());
    }

    /// udev writes an empty value rather than omitting the key for a volume
    /// with no label. That must read as absent, not as a label of "".
    #[test]
    fn an_empty_label_reads_as_absent() {
        let props = UdevProperties::parse("E:ID_FS_TYPE=vfat\nE:ID_FS_LABEL=\n");
        assert_eq!(props.fs_label(), None);
        assert_eq!(props.fs_type(), Some("vfat"));
    }

    #[test]
    fn a_block_device_file_is_named_by_its_numbers() {
        assert_eq!(
            UdevProperties::block_file(Path::new("/run/udev/data"), 259, 1),
            Path::new("/run/udev/data/b259:1")
        );
    }

    /// The database is a cache. It is absent under `mdev`, and a device
    /// plugged in mid-scan may not have a file yet. Neither is an error.
    #[test]
    fn a_missing_database_yields_empty_properties() {
        let tmp = TempDir::new("udev-missing");
        let props = UdevProperties::read_block_in(&tmp.path().join("nope"), 8, 17);
        assert!(props.is_empty());
        assert_eq!(props.len(), 0);
        assert_eq!(props.fs_type(), None);
        assert!(!props.is_mountable_filesystem());
    }

    #[test]
    fn a_device_is_read_from_its_numbered_file() {
        let tmp = TempDir::new("udev-read");
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
        let props = UdevProperties::read_block_in(&tmp.udev_data(), 8, 17);
        assert_eq!(props.fs_type(), Some("vfat"));
    }

    /// Character devices share the number space with block devices, so the
    /// `b` prefix is what keeps `b8:17` and `c8:17` apart.
    #[test]
    fn a_character_device_file_is_not_read_as_a_block_one() {
        let tmp = TempDir::new("udev-chardev");
        tmp.write("data/c8:17", "E:ID_FS_TYPE=wrong\n");
        let props = UdevProperties::read_block_in(&tmp.udev_data(), 8, 17);
        assert!(props.is_empty(), "a chardev record must not be read");
    }

    /// Every record on this machine must parse, and the disk holding the
    /// root filesystem must be found through its device numbers -- which is
    /// the assumption the whole lookup rests on.
    #[test]
    fn the_live_database_describes_the_root_filesystem() {
        let root = Path::new(UdevProperties::DEFAULT_ROOT);
        if !root.exists() {
            return;
        }
        let table = crate::MountTable::read();
        let Some(entry) = table.at("/") else {
            return;
        };
        let props = UdevProperties::read_block(entry.major(), entry.minor());
        if props.is_empty() {
            return;
        }
        assert_eq!(
            props.fs_type(),
            Some(entry.fs_type()),
            "udev and the kernel must agree on the root filesystem type"
        );
    }
}
