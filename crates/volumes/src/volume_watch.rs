//! [`VolumeWatch`] -- notice devices appearing and disappearing, cheaply.
//!
//! # Why polling, and why this is not as bad as it sounds
//!
//! `/proc/self/mountinfo` supports `poll(2)` with `POLLPRI`: the kernel wakes
//! a waiter when the mount table changes. That is the right mechanism, and it
//! is what a dedicated thread would use. It is not what this does, because
//! the file browser's event loop already wakes every 250ms and adding a
//! thread to a single-threaded painter buys nothing but a synchronisation
//! problem.
//!
//! What makes the polling acceptable is that the expensive part is skipped
//! almost always:
//!
//! - A poll first reads the tables and compares the **bytes** to the previous
//!   read. On an unchanged system that is two `read(2)`s of a few kilobytes.
//! - Only when the bytes differ does it parse, walk `/sys`, resolve symlinks
//!   and scan `/dev/disk/by-label` -- the part that costs real syscalls.
//!
//! The kernel rewrites `mountinfo` on every read, so its mtime is useless as
//! a change signal; the byte comparison is the cheap test available.
//!
//! # Two tables, because a device changes state in two ways
//!
//! The mount table alone cannot see a stick that is plugged in and left
//! unmounted -- it has no mount entry to appear in. That device is exactly
//! the one a Mount action exists for, so the watch compares
//! [`SystemTables::read_device_text`], which is the mount table *and* the
//! kernel's block-device list. Plugging in moves the second; mounting moves
//! the first.
//!
//! # Why the interval is not the loop interval
//!
//! The event loop's 250ms timeout is a safety net against a missed wakeup,
//! not a clock, and it also fires on every keystroke and pointer motion.
//! Polling on each pass would read the mount table while the user drags a
//! scrollbar. [`Self::due`] therefore gates on elapsed time.

use crate::device_scan::DeviceScan;
use crate::system_tables::SystemTables;
use crate::volume_set::VolumeSet;
use std::time::{Duration, Instant};

/// What a poll found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeChange {
    /// The mount table was byte-identical; nothing was parsed.
    Unchanged,
    /// The table changed but the listable volumes did not.
    ///
    /// Common and deliberately distinguished: a snap mount or a container
    /// starting rewrites `mountinfo` without touching removable storage, and
    /// repainting the sidebar for it would be wasted work.
    Irrelevant,
    /// The set of devices changed and the sidebar must be rebuilt.
    Changed,
}

impl VolumeChange {
    /// Whether the caller must rebuild and repaint.
    #[must_use]
    pub const fn needs_redraw(self) -> bool {
        matches!(self, Self::Changed)
    }
}

/// Watches the kernel tables for device changes.
pub struct VolumeWatch {
    /// The last raw table text, for the cheap change test.
    seen: String,
    /// The mounted volumes derived from it.
    volumes: VolumeSet,
    /// Every attached device, mounted or not.
    devices: DeviceScan,
    /// When the last poll ran.
    last_poll: Instant,
    /// How long to wait between polls.
    interval: Duration,
    /// Which kernel tables to read.
    tables: SystemTables,
}

impl VolumeWatch {
    /// How long between polls of the mount table.
    ///
    /// One second: plugging in a stick is a physical act, and the delay
    /// between the plug and the row appearing is dominated by the kernel
    /// probing the device and the automounter mounting it. A tighter poll
    /// would not make the row appear sooner.
    pub const INTERVAL: Duration = Duration::from_secs(1);

    /// Start watching the live mount table.
    #[must_use]
    pub fn new() -> Self {
        Self::watching(SystemTables::live(), Self::INTERVAL)
    }

    /// Watch arbitrary tables at an arbitrary interval, for testing.
    #[must_use]
    pub fn watching(tables: SystemTables, interval: Duration) -> Self {
        let seen = tables.read_device_text();
        let volumes = tables.volumes();
        let devices = tables.devices();
        Self {
            seen,
            volumes,
            devices,
            last_poll: Instant::now(),
            interval,
            tables,
        }
    }

    /// Whether enough time has passed to poll again.
    ///
    /// Checked by the caller before [`Self::poll`] so the common path -- an
    /// event loop pass triggered by a keystroke -- costs one clock read
    /// rather than a file read.
    #[must_use]
    pub fn due(&self) -> bool {
        self.last_poll.elapsed() >= self.interval
    }

    /// Poll the kernel tables, rebuilding only if they changed.
    ///
    /// Both scans are rebuilt whenever *either* differs, so the mounted set
    /// and the attached set are always from one moment. Refreshing only the
    /// one that changed would let a stick be listed as unmounted by one and
    /// mounted by the other, and the sidebar reads both to draw one row.
    pub fn poll(&mut self) -> VolumeChange {
        self.last_poll = Instant::now();
        let text = self.tables.read_device_text();
        if text == self.seen {
            return VolumeChange::Unchanged;
        }
        self.seen = text;
        let volumes = self.tables.volumes();
        let devices = self.tables.devices();
        if volumes.differs_from(&self.volumes) || devices.differs_from(&self.devices) {
            self.volumes = volumes;
            self.devices = devices;
            return VolumeChange::Changed;
        }
        VolumeChange::Irrelevant
    }

    /// Poll only if the interval has elapsed.
    pub fn poll_if_due(&mut self) -> VolumeChange {
        if self.due() {
            return self.poll();
        }
        VolumeChange::Unchanged
    }

    /// The mounted volumes as of the last poll.
    #[must_use]
    pub const fn volumes(&self) -> &VolumeSet {
        &self.volumes
    }

    /// Every attached device as of the last poll, mounted or not.
    #[must_use]
    pub const fn devices(&self) -> &DeviceScan {
        &self.devices
    }
}

impl Default for VolumeWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{VolumeChange, VolumeWatch};
    use crate::system_tables::SystemTables;
    use crate::test_support::{fixtures, TempDir};
    use std::path::Path;
    use std::time::Duration;

    const USB_BUS: &str = "pci0000:00/0000:00:14.0/usb2/2-1/2-1:1.0/host6/target6:0:0/6:0:0:0";
    const SD_BUS: &str = "pci0000:00/0000:00:1c.0/mmc_host/mmc0/mmc0:0001";
    const SATA_BUS: &str = "pci0000:00/0000:00:17.0/ata1/host0/target0:0:0/0:0:0:0";

    /// A fake system with a USB stick, an SD card and an optical drive, so
    /// the fixture mount tables resolve to real rows.
    fn system(label: &str) -> TempDir {
        let tmp = TempDir::new(label);
        tmp.block_disk(USB_BUS, "sdb", false);
        tmp.block_partition(USB_BUS, "sdb", "sdb1", 1);
        tmp.block_disk(SD_BUS, "mmcblk0", false);
        tmp.block_partition(SD_BUS, "mmcblk0", "mmcblk0p1", 1);
        tmp.block_disk(SATA_BUS, "sr0", true);
        tmp
    }

    /// A watch over a system the test controls, polling with no delay.
    fn watch(tmp: &TempDir, text: &str) -> VolumeWatch {
        tmp.write("mountinfo", text);
        VolumeWatch::watching(SystemTables::rooted_at(tmp.path()), Duration::ZERO)
    }

    #[test]
    fn an_unchanged_table_is_not_reparsed() {
        let tmp = system("watch-stable");
        let mut watch = watch(&tmp, fixtures::DESKTOP);
        assert_eq!(watch.poll(), VolumeChange::Unchanged);
        assert!(!watch.poll().needs_redraw());
    }

    /// Guards every other test here: if the fixture system resolved to no
    /// volumes at all, the change assertions below would pass vacuously
    /// while detecting nothing.
    #[test]
    fn the_fixture_system_actually_lists_devices() {
        let tmp = system("watch-guard");
        let watch = watch(&tmp, fixtures::DESKTOP);
        assert_eq!(
            watch.volumes().len(),
            3,
            "the stick, the card and the disc must all resolve"
        );
    }

    /// The case that must not repaint: a container or snap mount rewrites
    /// the table without touching removable storage.
    #[test]
    fn a_change_that_touches_no_device_is_irrelevant() {
        let tmp = system("watch-irrelevant");
        let mut watch = watch(&tmp, fixtures::DESKTOP);
        let before = watch.volumes().len();
        tmp.write(
            "mountinfo",
            &format!(
                "{desktop}70 25 0:60 / /snap/thing ro,relatime - squashfs /dev/loop9 ro\n",
                desktop = fixtures::DESKTOP
            ),
        );
        let change = watch.poll();
        assert_eq!(change, VolumeChange::Irrelevant);
        assert!(!change.needs_redraw(), "a loop mount must not repaint");
        assert_eq!(watch.volumes().len(), before, "the devices are untouched");
    }

    #[test]
    fn unplugging_a_device_is_a_change() {
        let tmp = system("watch-unplug");
        let mut watch = watch(&tmp, fixtures::DESKTOP);
        tmp.write("mountinfo", fixtures::DESKTOP_WITHOUT_USB);
        assert!(watch.poll().needs_redraw());
        assert!(
            watch
                .volumes()
                .at(std::path::Path::new("/run/media/user/USB"))
                .is_none(),
            "the unplugged stick must be gone from the set"
        );
    }

    /// The gap this watch could not see before: a stick that is plugged in
    /// and left unmounted writes no mount entry at all, so a watch comparing
    /// only the mount table would never notice it and the sidebar would
    /// never offer to mount it.
    #[test]
    fn plugging_in_an_unmounted_stick_is_a_change() {
        let tmp = system("watch-attach");
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        tmp.write("partitions", "major minor  #blocks  name\n");
        let mut watch = watch(&tmp, fixtures::DESKTOP_WITHOUT_USB);
        assert!(watch.devices().is_empty(), "udev knows nothing yet");
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
        tmp.write("partitions", "major minor  #blocks  name\n   8 17  976896 sdb1\n");
        assert!(watch.poll().needs_redraw(), "the new stick must be noticed");
        assert_eq!(watch.devices().mountable().len(), 1);
        assert!(
            watch.volumes().at(Path::new("/run/media/user/USB")).is_none(),
            "and it is still not mounted"
        );
    }

    /// Mounting that same stick must register too, or the row would go on
    /// offering a Mount action for a device that is already attached.
    #[test]
    fn mounting_an_attached_stick_is_a_change() {
        let tmp = system("watch-attach-mount");
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
        tmp.write("partitions", "major minor  #blocks  name\n   8 17  976896 sdb1\n");
        let mut watch = watch(&tmp, fixtures::DESKTOP_WITHOUT_USB);
        assert_eq!(watch.devices().mountable().len(), 1);
        tmp.write("mountinfo", fixtures::DESKTOP);
        assert!(watch.poll().needs_redraw());
        assert!(
            watch.devices().mountable().is_empty(),
            "a mounted stick must stop offering the action"
        );
    }

    /// The two scans are rebuilt together, so a row cannot be drawn from a
    /// mounted set and an attached set read at different moments.
    #[test]
    fn both_scans_come_from_one_moment() {
        let tmp = system("watch-coherent");
        tmp.sized(USB_BUS, "sdb", Some("sdb1"), 8, 17, 1_953_792);
        tmp.udev_block(8, 17, fixtures::UDEV_VFAT_PARTITION);
        tmp.write("partitions", "major minor  #blocks  name\n   8 17  976896 sdb1\n");
        let watch = watch(&tmp, fixtures::DESKTOP);
        let mounted_here = watch.volumes().at(Path::new("/run/media/user/USB")).is_some();
        let attached_mounted = watch
            .devices()
            .by_source("/dev/sdb1")
            .is_some_and(crate::AttachedDevice::is_mounted);
        assert!(mounted_here, "the fixture stick is mounted");
        assert_eq!(
            mounted_here, attached_mounted,
            "the two scans must agree about the same stick"
        );
    }

    #[test]
    fn plugging_a_device_in_is_a_change() {
        let tmp = system("watch-plug");
        let mut watch = watch(&tmp, fixtures::DESKTOP_WITHOUT_USB);
        tmp.write("mountinfo", fixtures::DESKTOP);
        assert!(watch.poll().needs_redraw());
        assert!(
            watch
                .volumes()
                .at(std::path::Path::new("/run/media/user/USB"))
                .is_some(),
            "the new stick must appear in the set"
        );
    }

    /// After reporting a change the watch must settle, or the window
    /// repaints forever.
    #[test]
    fn a_change_is_reported_once_and_then_settles() {
        let tmp = system("watch-settle");
        let mut watch = watch(&tmp, fixtures::DESKTOP);
        tmp.write("mountinfo", fixtures::DESKTOP_WITHOUT_USB);
        assert!(watch.poll().needs_redraw());
        assert_eq!(watch.poll(), VolumeChange::Unchanged);
        assert_eq!(watch.poll(), VolumeChange::Unchanged);
    }

    /// The interval gate is the whole reason this is affordable in an event
    /// loop that wakes on every keystroke.
    #[test]
    fn a_poll_before_the_interval_elapses_does_nothing() {
        let tmp = system("watch-interval");
        tmp.write("mountinfo", fixtures::DESKTOP);
        let mut watch = VolumeWatch::watching(
            SystemTables::rooted_at(tmp.path()),
            Duration::from_hours(1),
        );
        tmp.write("mountinfo", fixtures::DESKTOP_WITHOUT_USB);
        assert!(!watch.due());
        assert_eq!(watch.poll_if_due(), VolumeChange::Unchanged);
        assert!(
            watch.poll().needs_redraw(),
            "an explicit poll still sees the change"
        );
    }

    #[test]
    fn a_zero_interval_is_always_due() {
        let tmp = system("watch-due");
        let watch = watch(&tmp, fixtures::DESKTOP);
        assert!(watch.due());
    }

    /// A vanishing mount table must read as "no devices", not panic.
    #[test]
    fn an_unreadable_table_lists_nothing() {
        let tmp = TempDir::new("watch-unreadable");
        let watch =
            VolumeWatch::watching(SystemTables::rooted_at(tmp.path()), Duration::ZERO);
        assert!(watch.volumes().is_empty());
    }

    /// The live watch must construct and poll without error on this machine.
    #[test]
    fn the_live_watch_polls_without_error() {
        let mut watch = VolumeWatch::new();
        assert!(!watch.poll().needs_redraw(), "nothing changed in microseconds");
    }
}
