//! The send side: assembling a `SCM_RIGHTS` control buffer and the
//! [`MsgHdr`] that points at it.

use std::os::fd::{AsRawFd, BorrowedFd, RawFd};

use ququmatz::types::{IoVec, MsgHdr};

use crate::cmsg::{CMSG_HDR_LEN, CmsgAlign, CmsgHdr, SCM_RIGHTS, SOL_SOCKET};

/// A `sendmsg`/`recvmsg` message: a data buffer plus an optional `SCM_RIGHTS` control buffer.
///
/// Owns its `iovec` and control bytes so the pointers in the [`MsgHdr`] it
/// produces stay valid as long as `self` is alive — the caller must keep it
/// alive until the `io_uring` SENDMSG completes.
pub struct OutgoingMessage {
    iov: IoVec,
    control: Vec<u8>,
}

impl OutgoingMessage {
    /// Build a message that sends `bytes`, attaching `fds` as `SCM_RIGHTS`
    /// ancillary data. `bytes` must outlive the message (we borrow it into the
    /// iovec).
    #[must_use]
    pub fn new(bytes: &[u8], fds: &[BorrowedFd<'_>]) -> Self {
        // SAFETY: `bytes` outlives the message per the contract above; IoVec
        // borrows the pointer/len for the duration of the send.
        let iov = unsafe { IoVec::new(bytes.as_ptr().cast_mut(), bytes.len()) };

        let control = if fds.is_empty() {
            Vec::new()
        } else {
            let fd_bytes = std::mem::size_of::<RawFd>() * fds.len();
            let total = (CMSG_HDR_LEN + fd_bytes).cmsg_aligned();
            let mut buf = vec![0u8; total];
            // SAFETY: `buf` is sized for the header + fd payload computed above;
            // we write exactly that layout.
            unsafe {
                // The cast itself never gets dereferenced typed -- only fed
                // to `write_unaligned` below -- so the pointer's alignment
                // genuinely doesn't matter here.
                #[allow(clippy::cast_ptr_alignment)]
                let cmsg = buf.as_mut_ptr().cast::<CmsgHdr>();
                cmsg.write_unaligned(CmsgHdr {
                    len: CMSG_HDR_LEN + fd_bytes,
                    level: SOL_SOCKET,
                    kind: SCM_RIGHTS,
                });
                let data = buf.as_mut_ptr().add(CMSG_HDR_LEN).cast::<RawFd>();
                for (i, fd) in fds.iter().enumerate() {
                    data.add(i).write(fd.as_raw_fd());
                }
            }
            buf
        };

        Self { iov, control }
    }

    /// Produce a [`MsgHdr`] referencing this message's iovec and control
    /// buffer. The returned header borrows `self`; keep `self` alive until the
    /// send completes.
    pub fn header(&mut self) -> MsgHdr {
        let mut hdr = MsgHdr::default();
        hdr.msg_iov = &raw mut self.iov;
        hdr.msg_iovlen = 1;
        if self.control.is_empty() {
            hdr.msg_control = std::ptr::null_mut();
            hdr.msg_controllen = 0;
        } else {
            hdr.msg_control = self.control.as_mut_ptr();
            hdr.msg_controllen = self.control.len();
        }
        hdr
    }

    /// The assembled control region, for a caller that must inspect or
    /// re-parse the bytes it just built (the round-trip tests do exactly
    /// this, and it saves them reconstructing the slice through the raw
    /// pointers in [`Self::header`]).
    #[must_use]
    pub fn control_bytes(&self) -> &[u8] {
        &self.control
    }
}
