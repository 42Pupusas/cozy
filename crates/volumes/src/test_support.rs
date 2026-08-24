//! Test fixtures and scratch directories for this crate.
//!
//! The fixtures matter more here than in most crates. The machine running
//! these tests has exactly one internal `NVMe` disk and no removable media, so
//! *every* interesting case -- a USB stick, an SD card, a read-only optical
//! disc, an unnamed volume, a device that vanishes mid-session -- can only be
//! covered by feeding the parser text the kernel would have written.
//!
//! The live tables are still read in a handful of tests, to check that the
//! format assumptions hold against a real kernel. Fixtures prove the rules;
//! the live reads prove the fixtures are not fiction.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Mount-table text covering hardware this machine does not have.
pub mod fixtures {
    /// A desktop with a btrfs root, a separate `/home`, an EFI partition, a
    /// USB stick, an SD card, and the usual pseudo-filesystem noise.
    pub const DESKTOP: &str = "\
21 25 0:20 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
22 25 0:21 / /sys rw,nosuid,nodev,noexec,relatime shared:2 - sysfs sysfs rw
23 25 0:5 / /dev rw,nosuid shared:8 - devtmpfs devtmpfs rw,size=32524232k
25 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,compress=zstd:3
26 25 0:33 /@home /home rw,noatime shared:3 - btrfs /dev/nvme0n1p2 rw,subvol=/@home
27 25 0:29 / /tmp rw,nosuid,nodev shared:9 - tmpfs tmpfs rw,inode64
28 25 259:1 / /boot rw,relatime shared:4 - vfat /dev/nvme0n1p1 rw,fmask=0022
29 25 0:44 / /run/user/1000 rw,nosuid,nodev relatime shared:5 - tmpfs tmpfs rw,mode=700
40 25 8:17 / /run/media/user/USB rw,nosuid,nodev,relatime shared:6 - vfat /dev/sdb1 rw,uid=1000
41 25 179:1 / /run/media/user/SDCARD rw,nosuid,nodev,relatime shared:7 - exfat /dev/mmcblk0p1 rw
42 25 11:0 / /run/media/user/AUDIO ro,nosuid,nodev,relatime shared:10 - iso9660 /dev/sr0 ro
43 25 0:52 / /tmp/.mount_appimg ro,nosuid,nodev,relatime shared:11 - fuse.AppImage AppImage ro,user_id=1000
";

    /// The same desktop after the USB stick is unplugged.
    pub const DESKTOP_WITHOUT_USB: &str = "\
21 25 0:20 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
25 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p2 rw,compress=zstd:3
26 25 0:33 /@home /home rw,noatime shared:3 - btrfs /dev/nvme0n1p2 rw,subvol=/@home
28 25 259:1 / /boot rw,relatime shared:4 - vfat /dev/nvme0n1p1 rw,fmask=0022
41 25 179:1 / /run/media/user/SDCARD rw,nosuid,nodev,relatime shared:7 - exfat /dev/mmcblk0p1 rw
";

    /// A laptop with a LUKS-encrypted root on device-mapper and an external
    /// USB drive mounted under `/media`, which is where `pmount` and older
    /// tools put things.
    pub const ENCRYPTED_LAPTOP: &str = "\
25 1 254:0 / / rw,relatime shared:1 - ext4 /dev/mapper/cryptroot rw
26 25 8:1 / /boot rw,relatime shared:2 - ext2 /dev/sda1 rw
40 25 8:33 / /media/backup rw,relatime shared:3 - ext4 /dev/sdc1 rw
";
}

/// Monotonic counter keeping scratch directories unique across parallel
/// tests, as in `crates/fsmodel`.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely-named temporary directory, removed recursively on drop.
///
/// Used to build fake `/sys/class/block` trees, so device classification can
/// be tested against hardware layouts this machine does not have.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a scratch directory tagged with `label`.
    pub fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "volumes-test-{label}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    /// The scratch directory's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write a file, creating parent directories as needed.
    pub fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, contents).expect("write file");
        full
    }

    /// The fake `/sys/class/block` directory.
    ///
    /// Callers pass this to `DeviceClass::of_device_in`, exactly as
    /// production passes the real one.
    pub fn class_block(&self) -> PathBuf {
        self.path.join("class/block")
    }

    /// Build a fake disk, modelling how real sysfs is laid out.
    ///
    /// This is not a flat directory of device names. `/sys/class/block/sdb`
    /// is a **symlink** into `/sys/devices/...`, where the bus topology
    /// lives. Both facts matter: the bus check resolves the link to read the
    /// topology, and the partition lookup resolves it to find the owning
    /// disk. A flat fixture would let broken code pass.
    ///
    /// `bus_path` is the device-tree path under `devices`, e.g.
    /// `pci0000:00/0000:00:14.0/usb2/2-1` for a USB drive.
    pub fn block_disk(&self, bus_path: &str, disk: &str, removable: bool) {
        let node = format!("devices/{bus_path}/block/{disk}");
        let flag = u8::from(removable);
        self.write(&format!("{node}/removable"), &format!("{flag}\n"));
        self.link_into_class(&node, disk);
    }

    /// Build a fake partition of a disk created by [`Self::block_disk`].
    ///
    /// A partition directory sits *inside* its disk's directory and carries a
    /// `partition` file. It has no `removable` file of its own -- that lives
    /// on the parent disk, which is the subtlety being tested.
    pub fn block_partition(&self, bus_path: &str, disk: &str, partition: &str, number: u32) {
        let node = format!("devices/{bus_path}/block/{disk}/{partition}");
        self.write(&format!("{node}/partition"), &format!("{number}\n"));
        self.link_into_class(&node, partition);
    }

    /// Symlink `class/block/<name>` to a node in the fake device tree.
    fn link_into_class(&self, node: &str, name: &str) {
        let class = self.class_block();
        std::fs::create_dir_all(&class).expect("create class/block");
        let link = class.join(name);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(self.path.join(node), link).expect("symlink into class");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
