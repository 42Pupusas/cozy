//! The receive-side message: a payload buffer plus the control region the
//! kernel writes `SCM_RIGHTS` descriptors into.

use std::marker::PhantomData;

use ququmatz::types::{IoVec, MsgHdr};

use crate::incoming::IncomingControl;

/// `MSG_CTRUNC` — the kernel sets this in `msg_flags` when the control
/// region was too small and ancillary data was discarded.
const MSG_CTRUNC: i32 = 0x8;

/// A `recvmsg` message: a caller-owned payload buffer plus a control buffer
/// wide enough for `max_fds` descriptors.
///
/// The mirror of [`OutgoingMessage`](crate::OutgoingMessage). It exists
/// because a `recvmsg` needs the [`MsgHdr`] to stay put while the kernel
/// writes back into it: the descriptor count lands in `msg_controllen` and
/// the truncation flag in `msg_flags`, so the header must be read *after*
/// the operation completes, not rebuilt.
pub struct IncomingMessage<'a> {
    iov: IoVec,
    control: Vec<u8>,
    hdr: MsgHdr,
    _buf: PhantomData<&'a mut [u8]>,
}

impl<'a> IncomingMessage<'a> {
    /// Prepare a receive into `buf`, reserving room for up to `max_fds`
    /// descriptors of ancillary data.
    #[must_use]
    pub fn new(buf: &'a mut [u8], max_fds: usize) -> Self {
        // SAFETY: `buf` is borrowed for `'a`, which outlives this message,
        // so the pointer and length stay valid for every receive done
        // through it.
        let iov = unsafe { IoVec::new(buf.as_mut_ptr(), buf.len()) };
        Self {
            iov,
            control: vec![0u8; IncomingControl::reserve_len(max_fds)],
            hdr: MsgHdr::default(),
            _buf: PhantomData,
        }
    }

    /// Point the header at this message's current buffer locations and hand
    /// back a pointer to it, for an `io_uring` RECVMSG SQE.
    ///
    /// The pointers are refreshed on every call rather than fixed once in
    /// [`Self::new`], so moving the message between preparing it and using
    /// it cannot leave the header pointing at a stale address.
    pub const fn as_msghdr(&mut self) -> *mut MsgHdr {
        self.hdr.msg_iov = &raw mut self.iov;
        self.hdr.msg_iovlen = 1;
        self.hdr.msg_control = self.control.as_mut_ptr();
        self.hdr.msg_controllen = self.control.len();
        self.hdr.msg_flags = 0;
        &raw mut self.hdr
    }

    /// Take ownership of the descriptors the completed receive delivered.
    ///
    /// Reads `msg_controllen` as the kernel left it, so this must be called
    /// after the RECVMSG completes and before the next [`Self::as_msghdr`]
    /// resets it.
    #[must_use]
    pub fn take_fds(&self) -> Vec<std::os::fd::OwnedFd> {
        let written = self.hdr.msg_controllen.min(self.control.len());
        IncomingControl::fds(&self.control[..written])
    }

    /// Did the kernel discard ancillary data because the control region was
    /// too small?
    ///
    /// This is silent descriptor loss, so a caller must treat it as an error
    /// rather than continue with a short descriptor array.
    #[must_use]
    pub const fn control_truncated(&self) -> bool {
        self.hdr.msg_flags & MSG_CTRUNC != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_message_reports_no_truncation_and_no_fds() {
        let mut buf = [0u8; 32];
        let msg = IncomingMessage::new(&mut buf, 4);
        assert!(!msg.control_truncated());
        assert!(msg.take_fds().is_empty());
    }

    #[test]
    fn the_header_points_at_the_payload_and_control_buffers() {
        let mut buf = [0u8; 64];
        let mut msg = IncomingMessage::new(&mut buf, 2);
        let reserved = IncomingControl::reserve_len(2);
        // SAFETY: `as_msghdr` returns a pointer to a field of a live `msg`.
        let hdr = unsafe { &*msg.as_msghdr() };
        assert_eq!(hdr.msg_iovlen, 1);
        assert!(!hdr.msg_control.is_null());
        assert_eq!(hdr.msg_controllen, reserved);
    }

    #[test]
    fn reserving_more_descriptors_widens_the_control_region() {
        let mut small_buf = [0u8; 8];
        let mut big_buf = [0u8; 8];
        let mut small = IncomingMessage::new(&mut small_buf, 1);
        let mut big = IncomingMessage::new(&mut big_buf, 32);
        // SAFETY: both pointers refer to fields of live messages.
        let (small_hdr, big_hdr) = unsafe { (&*small.as_msghdr(), &*big.as_msghdr()) };
        assert!(big_hdr.msg_controllen > small_hdr.msg_controllen);
    }

    #[test]
    fn a_ctrunc_flag_from_the_kernel_is_reported() {
        let mut buf = [0u8; 8];
        let mut msg = IncomingMessage::new(&mut buf, 1);
        let _ = msg.as_msghdr();
        assert!(!msg.control_truncated());
        msg.hdr.msg_flags = MSG_CTRUNC;
        assert!(msg.control_truncated());
    }

    /// Preparing the header again must clear the previous receive's flags,
    /// or a single truncation would look permanent.
    #[test]
    fn preparing_the_header_again_clears_a_stale_truncation_flag() {
        let mut buf = [0u8; 8];
        let mut msg = IncomingMessage::new(&mut buf, 1);
        msg.hdr.msg_flags = MSG_CTRUNC;
        let _ = msg.as_msghdr();
        assert!(!msg.control_truncated());
    }
}
