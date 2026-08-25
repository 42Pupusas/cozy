//! The mount options a desktop may safely apply to untrusted media.
//!
//! # Why this is not a free-form string
//!
//! A removable disk is attacker-controlled data. Mounting one is done by a
//! privileged process, so the options decide whether plugging in a stick can
//! escalate to root:
//!
//! - `nosuid` stops a setuid-root binary on the stick from being honoured.
//!   Without it, anyone who can hand you a USB stick owns the machine.
//! - `nodev` stops a device node on the stick from granting raw access to
//!   real hardware, e.g. a crafted `/dev/sda` letting a user read a disk
//!   they have no permission to read.
//!
//! Neither is optional, so neither is expressible as "off": [`MountOptions`]
//! always emits both, and a caller-supplied option can never remove them.
//! Tests pin this, because a regression here is a local-root hole rather than
//! a misfeature.
//!
//! `noexec` is deliberately *not* forced. It breaks the ordinary case of
//! running a program from a stick, and unlike the other two it does not stop
//! a privilege escalation -- a user can always copy a binary off the stick
//! and run it from home.

/// Mount options for one filesystem, with the mandatory hardening applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOptions {
    parts: Vec<String>,
}

impl MountOptions {
    /// The options that are always present, whatever the caller asks for.
    const MANDATORY: [&'static str; 2] = ["nosuid", "nodev"];

    /// Options for `fs_type`, owned by `uid`.
    ///
    /// Filesystems with no concept of ownership (`vfat`, `exfat`, `ntfs`)
    /// need `uid=`/`gid=` or their contents mount as root-owned and the user
    /// cannot write to their own stick. Filesystems that carry real
    /// ownership (`ext4`, `btrfs`) must not be told a uid at all -- passing
    /// one is an error there, not a preference.
    #[must_use]
    pub fn for_filesystem(fs_type: &str, uid: u32, gid: u32) -> Self {
        let mut parts: Vec<String> = Self::MANDATORY.iter().map(|s| (*s).to_owned()).collect();
        if Self::needs_owner_hint(fs_type) {
            parts.push(format!("uid={uid}"));
            parts.push(format!("gid={gid}"));
        }
        if Self::is_fat(fs_type) {
            parts.push(String::from("utf8=1"));
            parts.push(String::from("shortname=mixed"));
        }
        Self { parts }
    }

    /// Read-only options, for a disk that must not be written.
    #[must_use]
    pub fn read_only(fs_type: &str, uid: u32, gid: u32) -> Self {
        let mut opts = Self::for_filesystem(fs_type, uid, gid);
        opts.parts.push(String::from("ro"));
        opts
    }

    /// The comma-separated option string systemd expects.
    #[must_use]
    pub fn as_string(&self) -> String {
        self.parts.join(",")
    }

    /// Whether `fs_type` stores no ownership of its own.
    fn needs_owner_hint(fs_type: &str) -> bool {
        matches!(
            fs_type,
            "vfat" | "exfat" | "msdos" | "ntfs" | "ntfs3" | "iso9660" | "udf" | "hfs" | "hfsplus"
        )
    }

    /// Whether `fs_type` is a FAT variant that wants name-mangling options.
    fn is_fat(fs_type: &str) -> bool {
        matches!(fs_type, "vfat" | "msdos")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that matters most: a setuid binary on a stick must never be
    /// honoured, whatever the filesystem.
    #[test]
    fn every_filesystem_is_mounted_nosuid_and_nodev() {
        for fs in [
            "vfat", "exfat", "ntfs", "ext4", "btrfs", "xfs", "iso9660", "f2fs", "",
        ] {
            let opts = MountOptions::for_filesystem(fs, 1000, 1000).as_string();
            assert!(opts.contains("nosuid"), "{fs} must be nosuid");
            assert!(opts.contains("nodev"), "{fs} must be nodev");
        }
    }

    /// The hardening must survive the read-only variant too.
    #[test]
    fn a_read_only_mount_is_still_hardened() {
        let opts = MountOptions::read_only("vfat", 1000, 1000).as_string();
        assert!(opts.contains("nosuid"));
        assert!(opts.contains("nodev"));
        assert!(opts.contains("ro"));
    }

    /// A FAT stick must end up owned by the user, or they cannot write to it.
    #[test]
    fn a_fat_filesystem_is_owned_by_the_mounting_user() {
        let opts = MountOptions::for_filesystem("vfat", 1000, 985).as_string();
        assert!(opts.contains("uid=1000"), "{opts}");
        assert!(opts.contains("gid=985"), "{opts}");
    }

    /// Passing a uid to a filesystem with real ownership is an error, not a
    /// preference, so it must be absent.
    #[test]
    fn a_posix_filesystem_is_given_no_owner_hint() {
        for fs in ["ext4", "btrfs", "xfs", "f2fs"] {
            let opts = MountOptions::for_filesystem(fs, 1000, 1000).as_string();
            assert!(!opts.contains("uid="), "{fs} must not carry a uid: {opts}");
            assert!(!opts.contains("gid="), "{fs} must not carry a gid: {opts}");
        }
    }

    /// An optical disc carries no ownership either, so it needs the hint.
    #[test]
    fn an_optical_filesystem_gets_an_owner_hint() {
        let opts = MountOptions::for_filesystem("iso9660", 1000, 1000).as_string();
        assert!(opts.contains("uid=1000"), "{opts}");
    }

    /// FAT name mangling keeps non-ASCII filenames readable.
    #[test]
    fn fat_gets_utf8_name_handling_and_other_filesystems_do_not() {
        assert!(
            MountOptions::for_filesystem("vfat", 1000, 1000)
                .as_string()
                .contains("utf8=1")
        );
        assert!(
            !MountOptions::for_filesystem("ext4", 1000, 1000)
                .as_string()
                .contains("utf8")
        );
    }

    /// The output must be a valid comma-separated list: no empty entries and
    /// no stray separators for the kernel to misparse.
    #[test]
    fn the_option_string_is_well_formed() {
        for fs in ["vfat", "ext4", "exfat"] {
            let opts = MountOptions::for_filesystem(fs, 1000, 1000).as_string();
            assert!(!opts.starts_with(','), "{opts}");
            assert!(!opts.ends_with(','), "{opts}");
            assert!(!opts.contains(",,"), "{opts}");
            assert!(opts.split(',').all(|p| !p.is_empty()), "{opts}");
        }
    }

    /// An unrecognised filesystem must still be hardened rather than falling
    /// back to bare options.
    #[test]
    fn an_unknown_filesystem_is_hardened_but_given_no_hint() {
        let opts = MountOptions::for_filesystem("some-future-fs", 1000, 1000).as_string();
        assert_eq!(opts, "nosuid,nodev");
    }
}
