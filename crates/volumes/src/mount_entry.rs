//! [`MountEntry`] -- one parsed line of `/proc/self/mountinfo`.
//!
//! `mountinfo` is used in preference to `/proc/mounts` for two reasons that
//! matter to a file browser:
//!
//! - It carries the **major:minor device number**, which is the only reliable
//!   way to tie a mount to its block device in `/sys`. Matching by device
//!   *name* fails for device-mapper, loop, and `NVMe` namespaces.
//! - It carries the **filesystem root**, which distinguishes a bind mount or
//!   a btrfs subvolume from a mount of the whole filesystem.
//!
//! # The format
//!
//! ```text
//! 40 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,compress=zstd
//! |  | |    |  | |          |        | |     |              |
//! |  | |    |  | |          |        | |     |              `- super options
//! |  | |    |  | |          |        | |     `- mount source
//! |  | |    |  | |          |        | `- filesystem type
//! |  | |    |  | |          |        `- separator
//! |  | |    |  | |          `- optional fields, ZERO OR MORE
//! |  | |    |  | `- mount options
//! |  | |    |  `- mount point
//! |  | |    `- root within the filesystem
//! |  | `- major:minor
//! |  `- parent mount ID
//! `- mount ID
//! ```
//!
//! The optional-fields run is the trap: it has **variable length**, so the
//! fields after it cannot be addressed by a fixed index. They must be located
//! by scanning for the `-` separator. Code that assumes a fixed layout works
//! on a typical desktop and breaks on the first shared or propagated mount.

/// One mount, as the kernel reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountEntry {
    /// Major device number.
    major: u32,
    /// Minor device number.
    minor: u32,
    /// The path *within* the filesystem that is mounted.
    ///
    /// `/` for a whole filesystem; a subpath for a bind mount or a btrfs
    /// subvolume such as `/@home`.
    root: String,
    /// Where the filesystem is attached in the directory tree.
    mount_point: String,
    /// Filesystem type: `ext4`, `btrfs`, `vfat`, `proc`, ...
    fs_type: String,
    /// What was mounted: a device path, or a pseudo-source like `proc`.
    source: String,
    /// Whether the mount is read-only.
    read_only: bool,
}

impl MountEntry {
    /// Parse one line, or `None` if it is malformed.
    ///
    /// A malformed line is skipped rather than failing the whole read: this
    /// file is written by the kernel and read while mounts change underneath
    /// us, so robustness beats strictness.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split(' ');
        let _mount_id = fields.next()?;
        let _parent_id = fields.next()?;
        let (major, minor) = Self::device_numbers(fields.next()?)?;
        let root = Self::unescape(fields.next()?);
        let mount_point = Self::unescape(fields.next()?);
        let options = fields.next()?;

        // The optional fields are variable in number, so everything after
        // them must be found by scanning for the separator rather than by
        // index.
        let mut rest = fields.skip_while(|field| *field != "-");
        rest.next()?;

        let fs_type = rest.next()?.to_owned();
        let source = Self::unescape(rest.next()?);

        Some(Self {
            major,
            minor,
            root,
            mount_point,
            fs_type,
            source,
            read_only: Self::has_option(options, "ro"),
        })
    }

    /// Split a `major:minor` field into its two numbers.
    fn device_numbers(field: &str) -> Option<(u32, u32)> {
        let (major, minor) = field.split_once(':')?;
        Some((major.parse().ok()?, minor.parse().ok()?))
    }

    /// Whether a comma-separated option list contains `wanted`.
    ///
    /// Compares whole options so that `ro` does not match inside
    /// `errors=remount-ro`, which is present on read-*write* mounts.
    fn has_option(options: &str, wanted: &str) -> bool {
        options.split(',').any(|option| option == wanted)
    }

    /// Decode the kernel's octal escapes.
    ///
    /// Space, tab, newline and backslash are written as `\040`, `\011`,
    /// `\012` and `\134`, because the format is space-separated. A mount
    /// point like `/run/media/My Drive` arrives as `/run/media/My\040Drive`,
    /// so a browser that skips this step shows the escape to the user and
    /// fails to open the path.
    fn unescape(field: &str) -> String {
        if !field.contains('\\') {
            return field.to_owned();
        }
        let mut out = String::with_capacity(field.len());
        let mut chars = field.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            let digits: String = chars.clone().take(3).collect();
            match Self::octal_byte(&digits) {
                Some(byte) => {
                    out.push(byte as char);
                    for _ in 0..3 {
                        chars.next();
                    }
                }
                // A lone backslash is legal in a filename; keep it verbatim.
                None => out.push(c),
            }
        }
        out
    }

    /// Decode exactly three octal digits into a byte.
    fn octal_byte(digits: &str) -> Option<u8> {
        if digits.len() != 3 || !digits.chars().all(|c| ('0'..='7').contains(&c)) {
            return None;
        }
        u8::from_str_radix(digits, 8).ok()
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

    /// The path within the filesystem that is mounted.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Where the filesystem is attached.
    #[must_use]
    pub fn mount_point(&self) -> &str {
        &self.mount_point
    }

    /// The filesystem type.
    #[must_use]
    pub fn fs_type(&self) -> &str {
        &self.fs_type
    }

    /// What was mounted.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the mount is read-only.
    #[must_use]
    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    /// Whether the source is a real block device rather than a pseudo one.
    ///
    /// Judged by the path, because `proc`, `sysfs`, `cgroup2` and `tmpfs`
    /// name themselves in this field while a real device gives `/dev/...`.
    #[must_use]
    pub fn has_block_source(&self) -> bool {
        self.source.starts_with("/dev/")
    }

    /// The device name under `/sys/class/block`, e.g. `nvme0n1p2`.
    ///
    /// `None` when the source is not a `/dev` path. Derived from the source
    /// path rather than the device numbers because the caller uses it only
    /// for display and for a `/sys` lookup that tolerates a miss.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        self.source.strip_prefix("/dev/")
    }
}

#[cfg(test)]
mod tests {
    use super::MountEntry;

    /// A real line from a btrfs root, including an optional field.
    const BTRFS_ROOT: &str = "40 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,compress=zstd:3,ssd,subvol=/@";

    #[test]
    fn parses_every_field_of_a_real_line() {
        let entry = MountEntry::parse(BTRFS_ROOT).expect("a well-formed line");
        assert_eq!(entry.major(), 0);
        assert_eq!(entry.minor(), 33);
        assert_eq!(entry.root(), "/@");
        assert_eq!(entry.mount_point(), "/");
        assert_eq!(entry.fs_type(), "btrfs");
        assert_eq!(entry.source(), "/dev/nvme0n1p2");
        assert!(!entry.read_only());
    }

    /// The optional-fields run varies in length, so the fields after the `-`
    /// separator cannot be read at a fixed index. This is the single most
    /// likely way to get this format wrong.
    #[test]
    fn the_separator_is_found_whatever_the_optional_field_count() {
        let none = "40 1 0:33 /@ / rw - btrfs /dev/sda1 rw";
        let three = "40 1 0:33 /@ / rw shared:1 master:2 propagate_from:3 - btrfs /dev/sda1 rw";
        for line in [none, three] {
            let entry = MountEntry::parse(line).expect("parses");
            assert_eq!(entry.fs_type(), "btrfs", "line: {line}");
            assert_eq!(entry.source(), "/dev/sda1", "line: {line}");
        }
    }

    #[test]
    fn a_read_only_mount_is_recognised() {
        let line = "1 2 8:1 / /mnt ro,relatime - vfat /dev/sdb1 ro";
        assert!(MountEntry::parse(line).expect("parses").read_only());
    }

    /// `errors=remount-ro` appears on read-*write* ext4 mounts. A substring
    /// test would call the filesystem read-only and wrongly refuse writes.
    #[test]
    fn remount_ro_in_an_option_value_is_not_a_read_only_mount() {
        let line = "1 2 8:1 / /mnt rw,relatime - ext4 /dev/sdb1 rw,errors=remount-ro";
        assert!(!MountEntry::parse(line).expect("parses").read_only());
    }

    /// A space in a mount point is written `\040`. Without decoding, the
    /// browser would display the escape and fail to open the directory.
    #[test]
    fn octal_escapes_in_paths_are_decoded() {
        let line = "1 2 8:1 / /run/media/My\\040Drive rw - vfat /dev/sdb1 rw";
        let entry = MountEntry::parse(line).expect("parses");
        assert_eq!(entry.mount_point(), "/run/media/My Drive");
    }

    #[test]
    fn every_escaped_character_round_trips() {
        let line = "1 2 8:1 / /a\\040b\\011c\\012d\\134e rw - ext4 /dev/sdb1 rw";
        let entry = MountEntry::parse(line).expect("parses");
        assert_eq!(entry.mount_point(), "/a b\tc\nd\\e");
    }

    /// A backslash that does not introduce three octal digits is a literal
    /// character in a legal filename, and must survive unchanged.
    #[test]
    fn a_lone_backslash_is_left_alone() {
        let line = "1 2 8:1 / /a\\bc rw - ext4 /dev/sdb1 rw";
        assert_eq!(
            MountEntry::parse(line).expect("parses").mount_point(),
            "/a\\bc"
        );
    }

    #[test]
    fn a_pseudo_filesystem_has_no_block_source() {
        let line = "43 40 0:25 / /sys rw - sysfs sysfs rw";
        let entry = MountEntry::parse(line).expect("parses");
        assert!(!entry.has_block_source());
        assert_eq!(entry.device_name(), None);
    }

    #[test]
    fn a_block_source_yields_its_sys_device_name() {
        let entry = MountEntry::parse(BTRFS_ROOT).expect("parses");
        assert!(entry.has_block_source());
        assert_eq!(entry.device_name(), Some("nvme0n1p2"));
    }

    /// The file is read while mounts change underneath us, so a truncated or
    /// malformed line must be skipped rather than aborting the whole read.
    #[test]
    fn malformed_lines_are_skipped_rather_than_fatal() {
        for line in [
            "",
            "40",
            "40 1 0:33 /@ /",
            "40 1 notanumber /@ / rw - ext4 /dev/sda1 rw",
            "40 1 0:33 /@ / rw no-separator ext4 /dev/sda1 rw",
        ] {
            assert!(MountEntry::parse(line).is_none(), "should reject: {line:?}");
        }
    }

    /// The kernel writes this file; parsing every line of the live one is a
    /// cheap check that the format assumptions hold on this machine.
    #[test]
    fn every_line_of_the_live_mount_table_parses() {
        let text = std::fs::read_to_string("/proc/self/mountinfo").expect("readable on Linux");
        let mut count = 0;
        for line in text.lines() {
            assert!(
                MountEntry::parse(line).is_some(),
                "failed to parse a live line: {line:?}"
            );
            count += 1;
        }
        assert!(count > 0, "the mount table is never empty");
    }

    /// The root filesystem is always present, and is always a real device.
    #[test]
    fn the_live_table_contains_a_root_mount() {
        let text = std::fs::read_to_string("/proc/self/mountinfo").expect("readable");
        let root = text
            .lines()
            .filter_map(MountEntry::parse)
            .find(|entry| entry.mount_point() == "/")
            .expect("there is always a root mount");
        assert!(!root.fs_type().is_empty());
    }
}
