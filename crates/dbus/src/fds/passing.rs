//! Sending and receiving file descriptors over the bus socket.

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};

use ququmatz::types::MsgFlags;
use ququmatz::{IoUring, Sqe};
use scm_rights::{IncomingMessage, OutgoingMessage};

use super::queue::FdQueue;

/// Why a descriptor-carrying transfer failed.
///
/// Separate from `DbusError` so this module stays usable without the
/// connection type, and so the two FD-specific failures cannot be confused
/// with ordinary socket noise.
#[derive(Debug)]
pub enum FdError {
    /// The socket operation failed.
    Io(io::Error),
    /// The peer closed the connection.
    Closed,
    /// The kernel discarded ancillary data: the control region was too small
    /// for what arrived. Continuing would silently lose descriptors.
    ControlTruncated,
    /// Descriptors were offered for a connection whose peer never agreed to
    /// `NEGOTIATE_UNIX_FD`.
    NotNegotiated,
}

impl std::fmt::Display for FdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "descriptor-passing socket I/O failed: {e}"),
            Self::Closed => write!(f, "the peer closed the connection"),
            Self::ControlTruncated => {
                write!(f, "the kernel truncated ancillary data; descriptors lost")
            }
            Self::NotNegotiated => {
                write!(f, "the peer did not agree to unix FD passing")
            }
        }
    }
}

impl std::error::Error for FdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// The descriptor-passing half of a bus connection.
///
/// Owns the staging queue for received descriptors and performs the
/// `sendmsg`/`recvmsg` that carry `SCM_RIGHTS`. It deliberately does not own
/// the socket or the ring: `DbusConn` owns those and lends them per call, so
/// this type stays testable over a plain socket pair with no bus at all.
///
/// # Why a distinct type
///
/// The plain byte-stream path (`send`/`recv`) cannot carry ancillary data,
/// and the message path must consult negotiation state, stage descriptors,
/// and detect truncation. Keeping that here leaves `DbusConn` responsible
/// for framing and dispatch only.
#[derive(Debug)]
pub struct FdPassing {
    queue: FdQueue,
    negotiated: bool,
    max_fds: usize,
}

impl FdPassing {
    /// The D-Bus specification caps a message at 16 descriptors, and the
    /// reference implementation enforces exactly that. Reserving room for
    /// this many keeps any legal message from truncating.
    pub const MAX_FDS_PER_MESSAGE: usize = 16;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            queue: FdQueue::new(),
            negotiated: false,
            max_fds: Self::MAX_FDS_PER_MESSAGE,
        }
    }

    /// Record whether the peer answered `AGREE_UNIX_FD` during SASL.
    pub const fn set_negotiated(&mut self, negotiated: bool) {
        self.negotiated = negotiated;
    }

    /// Did the peer agree to descriptor passing?
    #[must_use]
    pub const fn is_negotiated(&self) -> bool {
        self.negotiated
    }

    /// Descriptors received but not yet claimed by a parsed message.
    #[must_use]
    pub fn staged_len(&self) -> usize {
        self.queue.staged_len()
    }

    /// Claim the `count` descriptors belonging to a completed message, in
    /// arrival order. `None` when fewer have arrived.
    #[must_use]
    pub fn claim(&mut self, count: usize) -> Option<Vec<OwnedFd>> {
        self.queue.claim(count)
    }

    /// Close every staged descriptor, for a connection abandoning a
    /// half-read message.
    pub fn discard_staged(&mut self) {
        self.queue.clear();
    }

    /// Send `bytes` with `fds` attached as `SCM_RIGHTS`.
    ///
    /// Returns the number of bytes accepted. The descriptors ride on the
    /// first byte of the send, so a caller looping over a partial write must
    /// attach them only to the first call.
    ///
    /// Fails with [`FdError::NotNegotiated`] rather than degrading to a
    /// plain send when the peer never agreed: a silently dropped descriptor
    /// becomes a caller that waits forever for a file it will never get.
    pub fn send_with_fds(
        &self,
        ring: &mut IoUring,
        sock: ququmatz::types::RawFd,
        bytes: &[u8],
        fds: &[BorrowedFd<'_>],
    ) -> Result<usize, FdError> {
        if !fds.is_empty() && !self.negotiated {
            return Err(FdError::NotNegotiated);
        }
        let mut msg = OutgoingMessage::new(bytes, fds);
        let hdr = msg.header();
        // SAFETY: `msg` owns the iovec and control buffer the header points
        // at, and outlives this blocking submit-and-wait.
        let sqe = unsafe { Sqe::sendmsg_ptr(sock, &raw const hdr, MsgFlags::NOSIGNAL) };
        let n = Self::run_one(ring, sqe)?;
        Ok(n)
    }

    /// Receive into `buf`, staging any descriptors that arrive.
    ///
    /// Returns the number of payload bytes read. `MSG_CMSG_CLOEXEC` is set
    /// so the kernel marks each received descriptor close-on-exec as it
    /// installs it; a later `fcntl` could not close the window where a
    /// concurrent fork leaks the descriptor to a child.
    pub fn recv_with_fds(
        &mut self,
        ring: &mut IoUring,
        sock: ququmatz::types::RawFd,
        buf: &mut [u8],
    ) -> Result<usize, FdError> {
        let mut msg = IncomingMessage::new(buf, self.max_fds);
        let hdr = msg.as_msghdr();
        // SAFETY: `msg` owns the buffers `hdr` points at and outlives this
        // blocking submit-and-wait.
        let sqe = unsafe { Sqe::recvmsg_ptr(sock, hdr, MsgFlags::CMSG_CLOEXEC) };
        let n = Self::run_one(ring, sqe)?;

        // Stage before the truncation check: descriptors that did arrive are
        // owned by this process either way, so dropping out early would leak
        // exactly the ones the kernel did manage to deliver.
        let fds = msg.take_fds();
        if !fds.is_empty() {
            self.queue.push_received(fds);
        }
        if msg.control_truncated() {
            return Err(FdError::ControlTruncated);
        }
        Ok(n)
    }

    /// Submit one SQE, block for its completion, and translate the result
    /// into a byte count.
    fn run_one(ring: &mut IoUring, sqe: Sqe) -> Result<usize, FdError> {
        const EINTR: i32 = 4;
        ring.push(sqe)
            .map_err(|e| FdError::Io(io::Error::other(format!("{e}"))))?;
        loop {
            match ring.submit_and_wait(1) {
                Ok(_) => break,
                Err(
                    ququmatz::Error::Syscall(errno)
                    | ququmatz::Error::Submit(ququmatz::SubmitError::Syscall(errno)),
                ) if errno.raw() == EINTR => {}
                Err(e) => return Err(FdError::Io(io::Error::other(format!("{e}")))),
            }
        }
        let cqe = ring
            .complete()
            .ok_or_else(|| FdError::Io(io::Error::other("no completion")))?;
        let result = cqe.raw_result();
        if result < 0 {
            return Err(FdError::Io(io::Error::from_raw_os_error(-result)));
        }
        if result == 0 {
            return Err(FdError::Closed);
        }
        Ok(usize::try_from(result).unwrap_or(0))
    }
}

impl Default for FdPassing {
    fn default() -> Self {
        Self::new()
    }
}
