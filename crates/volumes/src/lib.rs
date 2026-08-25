//! Storage-volume detection from the kernel's own tables.
//!
//! Answers one question for a file browser: **what storage is attached, and
//! where can I browse it?**
//!
//! # No daemon
//!
//! The usual answer on a Linux desktop is `UDisks2` over D-Bus. This crate does
//! not use it, because everything needed to *detect* a volume is already in
//! the kernel's tables:
//!
//! ```text
//! /proc/self/mountinfo    mount points, devices, fs types, ro/rw
//! /sys/class/block/*      the removable flag, size, whether it is virtual
//! /dev/disk/by-label/     human-readable volume names
//! /run/udev/data/b*       the filesystem on a device that is NOT mounted
//! ```
//!
//! Reading them directly means detection works on machines with no `UDisks2`
//! installed, adds no dependency, and stays unit-testable without a bus.
//!
//! # Mounted and merely attached
//!
//! These are two different questions, and they need two different scans.
//!
//! [`VolumeSet`] lists what is **mounted**, built from the mount table. It
//! cannot represent a stick that is plugged in and not mounted, because such
//! a device has no mount entry to build from -- and that is exactly the
//! device a Mount action is for.
//!
//! [`DeviceScan`] therefore walks the block devices themselves and asks udev
//! what filesystem each holds, producing an [`AttachedDevice`] whose mount
//! point is optional. The two scans join on **device numbers**, never on
//! device names, because a volume mounted through `/dev/disk/by-uuid/...`
//! names no `/sys` node at all.
//!
//! # What this crate deliberately does not do
//!
//! It does not **perform** a mount. `mount(2)` requires `CAP_SYS_ADMIN`, so
//! an unprivileged desktop application cannot do it without either a setuid
//! helper or a privileged service. Doing it here would mean either shipping a
//! local-root attack surface, or dragging D-Bus, systemd and polkit into a
//! crate whose whole value is having no dependencies.
//!
//! Instead it *describes* one. [`MountBackend`] is the seam: this crate says
//! which device to attach and where ([`MountRequest`]), and an implementation
//! elsewhere -- in the binary that already holds a bus connection -- performs
//! the privileged act and reports a [`MountOutcome`].
//!
//! The destination is the security-critical part, because it is derived from
//! a **label stored on the volume**, which is chosen by whoever formatted it
//! rather than by the user. [`MountPoint`] is the only way to name one, and
//! it cannot be built outside its own root -- see its documentation.
//!
//! The rest is about naming and listing storage, plus the one maintenance
//! action that genuinely needs no privilege: flushing pending writes
//! ([`VolumeFlush`]).

pub mod attached_device;
pub mod block_device;
pub mod device_class;
pub mod device_scan;
pub mod mount_backend;
pub mod mount_entry;
pub mod mount_point;
pub mod mount_request;
pub mod mount_table;
pub mod scripted_backend;
pub mod system_tables;
pub mod udev_properties;
pub mod volume;
pub mod volume_flush;
pub mod volume_set;
pub mod volume_watch;

#[cfg(test)]
mod test_support;

pub use attached_device::AttachedDevice;
pub use block_device::BlockDevice;
pub use device_class::DeviceClass;
pub use device_scan::DeviceScan;
pub use mount_backend::{MountBackend, MountOutcome};
pub use mount_entry::MountEntry;
pub use mount_point::MountPoint;
pub use mount_request::MountRequest;
pub use mount_table::MountTable;
pub use scripted_backend::ScriptedBackend;
pub use volume::Volume;
pub use system_tables::SystemTables;
pub use udev_properties::UdevProperties;
pub use volume_flush::{FlushError, VolumeFlush};
pub use volume_set::VolumeSet;
pub use volume_watch::{VolumeChange, VolumeWatch};
