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
//! ```
//!
//! Reading them directly means detection works on machines with no `UDisks2`
//! installed, adds no dependency, and stays unit-testable without a bus.
//!
//! # What this crate deliberately does not do
//!
//! It does not **mount**. `mount(2)` requires `CAP_SYS_ADMIN`, so an
//! unprivileged desktop application cannot do it without either a setuid
//! helper or a privileged daemon -- which is what `UDisks2` is. Pretending
//! otherwise would mean shipping a local-root attack surface.
//!
//! Everything here is therefore about volumes that are *already* mounted,
//! plus the one maintenance action that genuinely needs no privilege:
//! flushing pending writes ([`VolumeFlush`]).

pub mod device_class;
pub mod mount_entry;
pub mod mount_table;
pub mod system_tables;
pub mod volume;
pub mod volume_flush;
pub mod volume_set;
pub mod volume_watch;

#[cfg(test)]
mod test_support;

pub use device_class::DeviceClass;
pub use mount_entry::MountEntry;
pub use mount_table::MountTable;
pub use volume::Volume;
pub use system_tables::SystemTables;
pub use volume_flush::{FlushError, VolumeFlush};
pub use volume_set::VolumeSet;
pub use volume_watch::{VolumeChange, VolumeWatch};
