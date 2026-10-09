//! [`DeviceSignals`] -- the kernel's own notifications that devices changed.
//!
//! A device changes state in two ways, and each has its own signal:
//!
//! * **Mounting or unmounting** rewrites the mount table. An open
//!   `/proc/self/mountinfo` reports that as `POLLPRI` (with `POLLERR`) on the
//!   first poll after the change, so holding the file open is the watch.
//! * **Plugging in or pulling out** a device is not visible in the mount
//!   table while it stays unmounted, and `/proc/partitions` cannot be polled.
//!   What can be watched is udev's record for the device in
//!   `/run/udev/data/b<major>:<minor>`: it is written once udev has finished
//!   probing, which is the moment the scan can first read the filesystem
//!   type, and removed when the device goes. An inotify watch on that
//!   directory fires at exactly those two moments.
//!
//! Both are armed **before** the first scan, so a change that lands between
//! the scan and the first wait is reported rather than lost.

use crate::system_tables::SystemTables;
use inotify_events::InotifyEvents;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};

/// File descriptors that become ready when the device tables change.
pub struct DeviceSignals {
    mounts: Option<File>,
    udev: Option<ququmatz::Inotify>,
}

impl DeviceSignals {
    /// udev names block device records `b<major>:<minor>`; every other
    /// prefix is a character device, a network interface or similar, whose
    /// changes cannot alter the device list.
    const BLOCK_RECORD_PREFIX: u8 = b'b';

    /// Arm the signals on `tables`, best effort.
    ///
    /// A missing source is left unarmed and reported as `None` by its fd
    /// accessor: a machine without udev still sees mounts, and one without
    /// either still scans once at start.
    #[must_use]
    pub fn arm(tables: &SystemTables) -> Self {
        Self {
            mounts: File::open(tables.mountinfo()).ok(),
            udev: Self::watch_udev(tables),
        }
    }

    fn watch_udev(tables: &SystemTables) -> Option<ququmatz::Inotify> {
        let ino = ququmatz::Inotify::new().ok()?;
        let root = CString::new(tables.udev_root().as_os_str().as_encoded_bytes()).ok()?;
        let mask = ququmatz::WatchMask::CLOSE_WRITE
            | ququmatz::WatchMask::MOVED_TO
            | ququmatz::WatchMask::MOVED_FROM
            | ququmatz::WatchMask::DELETE;
        ino.add_watch(&root, mask).ok()?;
        Some(ino)
    }

    /// The mount table descriptor. Wait on it for `POLLPRI`.
    #[must_use]
    pub fn mounts_fd(&self) -> Option<RawFd> {
        self.mounts.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// The udev watch descriptor. Wait on it for readability.
    #[must_use]
    pub fn udev_fd(&self) -> Option<RawFd> {
        self.udev.as_ref().map(|ino| ino.fd().as_i32())
    }

    /// Drain the udev watch, reporting whether a block device record came
    /// or went. Must be called whenever [`Self::udev_fd`] is ready, or the
    /// descriptor stays readable.
    #[must_use]
    pub fn drain_block_events(&self) -> bool {
        let Some(ino) = &self.udev else {
            return false;
        };
        let mut block = false;
        InotifyEvents::poll_names(ino, |name| {
            block |= name.first() == Some(&Self::BLOCK_RECORD_PREFIX);
        });
        block
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceSignals;
    use crate::system_tables::SystemTables;
    use crate::test_support::TempDir;

    fn armed(tmp: &TempDir) -> DeviceSignals {
        std::fs::create_dir_all(tmp.path().join("data")).expect("udev dir");
        tmp.write("mountinfo", "");
        DeviceSignals::arm(&SystemTables::rooted_at(tmp.path()))
    }

    fn drained_after_writing(name: &str) -> bool {
        let tmp = TempDir::new("signals-write");
        let signals = armed(&tmp);
        std::fs::write(tmp.path().join("data").join(name), "E:ID_FS_TYPE=vfat\n").expect("write");
        signals.drain_block_events()
    }

    #[test]
    fn a_new_block_record_is_reported() {
        assert!(drained_after_writing("b8:17"));
    }

    #[test]
    fn a_record_for_another_kind_of_device_is_not() {
        assert!(!drained_after_writing("c13:64"));
        assert!(!drained_after_writing("n3"));
    }

    #[test]
    fn a_removed_block_record_is_reported() {
        let tmp = TempDir::new("signals-remove");
        std::fs::create_dir_all(tmp.path().join("data")).expect("udev dir");
        let record = tmp.path().join("data").join("b8:17");
        std::fs::write(&record, "x").expect("write");
        let signals = armed(&tmp);
        std::fs::remove_file(&record).expect("remove");
        assert!(signals.drain_block_events());
    }

    #[test]
    fn udev_replacing_a_record_by_rename_is_reported() {
        let tmp = TempDir::new("signals-rename");
        let signals = armed(&tmp);
        let data = tmp.path().join("data");
        std::fs::write(data.join(".#b8:17tmp"), "x").expect("temporary");
        std::fs::rename(data.join(".#b8:17tmp"), data.join("b8:17")).expect("rename");
        assert!(signals.drain_block_events());
    }

    #[test]
    fn draining_settles() {
        let tmp = TempDir::new("signals-settle");
        let signals = armed(&tmp);
        std::fs::write(tmp.path().join("data").join("b8:1"), "x").expect("write");
        assert!(signals.drain_block_events());
        assert!(!signals.drain_block_events());
    }

    #[test]
    fn a_system_without_udev_still_arms_the_mount_table() {
        let tmp = TempDir::new("signals-no-udev");
        tmp.write("mountinfo", "");
        let signals = DeviceSignals::arm(&SystemTables::rooted_at(tmp.path()));
        assert!(signals.udev_fd().is_none());
        assert!(signals.mounts_fd().is_some());
        assert!(!signals.drain_block_events());
    }

    #[test]
    fn the_live_mount_table_starts_quiet() {
        let signals = DeviceSignals::arm(&SystemTables::live());
        let fd = signals.mounts_fd().expect("mountinfo opens");
        let mut set = waker::PollSet::new();
        set.watch(fd, waker::Interest::Priority);
        assert!(!set.wait(0), "nothing has changed since it was opened");
    }
}
