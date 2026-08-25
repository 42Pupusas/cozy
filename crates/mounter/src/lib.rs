//! Mounting removable storage, for a desktop that holds no privilege.
//!
//! # What this crate is
//!
//! The implementation of `volumes::MountBackend`. `crates/volumes` decides
//! *which* device to attach and *where*; this decides *how*, and needs D-Bus,
//! systemd and polkit to do it. Keeping the two apart is what lets `volumes`
//! stay dependency-free and keeps the file browser from learning what a bus
//! is.
//!
//! # How a mount actually happens
//!
//! `mount(2)` needs `CAP_SYS_ADMIN`. The routes to a mounted stick are an
//! fstab `user` entry (which must name the device in advance), a setuid
//! helper of our own (a local-root attack surface we refuse to ship), or
//! asking a service that already has the privilege. This is the third:
//! systemd creates a transient `.mount` unit on request.
//!
//! `UDisks2` is the usual such service and is not installed everywhere --
//! notably not on the machine this was written on. systemd always is.
//!
//! # The prompt is not optional
//!
//! `org.freedesktop.systemd1.manage-units` is `auth_admin_keep`, so the call
//! is refused unless an authentication agent answers a password prompt. Two
//! things follow, both of which produced silent failures on the first live
//! run:
//!
//! - The caller must set `ALLOW_INTERACTIVE_AUTHORIZATION` on the method
//!   call, or the service refuses immediately and polkit never consults an
//!   agent at all.
//! - Somebody must *be* the agent. This machine has no graphical one.
//!
//! [`PromptingBackend`] does both: it registers as the session's agent,
//! retries the mount, and drives the password conversation through whatever
//! prompt the caller supplies.

pub mod prompting_backend;
pub mod systemd_mount;

pub use prompting_backend::{PasswordPrompt, PromptingBackend};
pub use systemd_mount::SystemdMount;
