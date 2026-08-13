//! `SCM_RIGHTS` ancillary-data assembly and parsing for `AF_UNIX` sockets.
//!
//! Passing a file descriptor between processes means attaching it to a
//! `sendmsg` as a control (cmsg) message, and reading it back off the
//! matching `recvmsg`. The byte layout is the same for every caller, so it
//! lives here once:
//!
//! * [`OutgoingMessage`] — build the control buffer and the [`MsgHdr`]
//!   pointing at it.
//! * [`IncomingMessage`] — the receive-side mirror: a payload buffer plus a
//!   control region sized for a descriptor count, holding the [`MsgHdr`]
//!   still so the kernel can write back into it.
//! * [`IncomingControl`] — take ownership of the descriptors the kernel
//!   wrote into a received control region.
//!
//! # Why its own crate
//!
//! This code was written for `crates/platform`, which sends the compositor a
//! shm pool descriptor, and moved to `crates/platform-sys` when that crate
//! was split. `crates/dbus` then needed the identical layout for D-Bus unix
//! FD passing (a portal returns real descriptors). Depending on
//! `platform-sys` for it would tie the bus client to the display stack for
//! sixty lines of `#[repr(C)]`, so both now share this leaf instead: it
//! needs only `std` and `ququmatz::types`, and knows nothing of Wayland or
//! D-Bus.
//!
//! The transmit itself is never done here. Callers point an `io_uring`
//! SENDMSG/RECVMSG SQE at the [`MsgHdr`], matching this workspace's policy
//! of routing all I/O through `io_uring`.
//!
//! [`MsgHdr`]: ququmatz::types::MsgHdr

mod cmsg;
mod incoming;
mod incoming_message;
mod outgoing;

pub use incoming::IncomingControl;
pub use incoming_message::IncomingMessage;
pub use outgoing::OutgoingMessage;

#[cfg(test)]
mod tests;
