//! Unix file-descriptor passing over the bus socket.
//!
//! D-Bus carries a descriptor as ancillary data (`SCM_RIGHTS`) riding on the
//! message that references it. The message body holds only an *index* (the
//! `h` type, [`crate::value::Value::UnixFd`]) into the array of descriptors
//! that arrived with it. Three pieces implement that:
//!
//! * [`FdPassing`] — the `sendmsg`/`recvmsg` seam, and the negotiation flag.
//! * [`queue::FdQueue`] — arrival-ordered staging, because a stream socket
//!   does not deliver ancillary data on message boundaries.
//! * [`socket_pair::SocketPair`] — a real connected pair, so the transport
//!   is tested against the kernel rather than a mock.
//!
//! The byte-stream path in [`crate::conn`] stays on plain `send`/`recv`.
//! Only connections that negotiated FD passing pay for the message path.

mod passing;
mod queue;
mod socket_pair;

#[cfg(test)]
mod conn_tests;
#[cfg(test)]
mod fd_frame;
#[cfg(test)]
mod test_fds;
#[cfg(test)]
mod tests;

pub use passing::{FdError, FdPassing};
pub use queue::FdQueue;
pub use socket_pair::SocketPair;
