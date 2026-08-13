//! The receive side: reading `SCM_RIGHTS` descriptors back out of a control
//! region the kernel wrote.

use std::os::fd::{OwnedFd, RawFd};

use crate::cmsg::{CMSG_HDR_LEN, CmsgAlign, CmsgHdr, SCM_RIGHTS, SOL_SOCKET};

/// The received counterpart to [`crate::OutgoingMessage`]: reading
/// `SCM_RIGHTS` ancillary data back out of a control (cmsg) region the
/// kernel wrote.
///
/// A zero-sized namespace rather than free functions at module scope -- same
/// reasoning as `pty::sys::RawPty`: there's no fd/message value in hand yet
/// to attach these to (they parse one), so the idiomatic home is an
/// associated function on a type that names the concept, not a bare
/// module-level fn.
pub struct IncomingControl;

impl IncomingControl {
    /// The fixed control-region width to reserve on every multishot recvmsg,
    /// sized to hold one `SCM_RIGHTS` cmsg carrying up to `max_fds`
    /// descriptors. This is the `msg_controllen` set on the persistent recv
    /// `MsgHdr` -- and thus the width of the control region inside each
    /// provided buffer.
    #[must_use]
    pub fn reserve_len(max_fds: usize) -> usize {
        (CMSG_HDR_LEN + std::mem::size_of::<RawFd>() * max_fds).cmsg_aligned()
    }

    /// Parse `SCM_RIGHTS` file descriptors out of a received control (cmsg)
    /// region, taking ownership of each. `control` is the control slice the
    /// kernel wrote -- e.g. `RecvmsgParts::control` from a multishot recvmsg
    /// buffer.
    ///
    /// The kernel may deliver several `SCM_RIGHTS` items in one control
    /// region, so this walks the whole cmsg chain rather than reading only
    /// the first header.
    #[must_use]
    pub fn fds(control: &[u8]) -> Vec<OwnedFd> {
        use std::os::fd::FromRawFd;
        let mut fds = Vec::new();
        let mut offset = 0usize;
        while offset + CMSG_HDR_LEN <= control.len() {
            // SAFETY: the loop condition guarantees CMSG_HDR_LEN readable
            // bytes at `offset`, which is a valid cmsghdr; the fd payload
            // behind it is bounded by `len`, checked against the slice
            // before it is read.
            let (level, kind, len) = unsafe {
                // Same unaligned-read rationale as the write side.
                #[allow(clippy::cast_ptr_alignment)]
                let cmsg = control.as_ptr().add(offset).cast::<CmsgHdr>();
                let hdr = cmsg.read_unaligned();
                (hdr.level, hdr.kind, hdr.len)
            };
            if len < CMSG_HDR_LEN || len > control.len() - offset {
                break;
            }
            if level == SOL_SOCKET && kind == SCM_RIGHTS {
                let count = (len - CMSG_HDR_LEN) / std::mem::size_of::<RawFd>();
                // SAFETY: `len` is within the slice (checked above), so the
                // `count` descriptors behind the header are readable. Each
                // is a fresh descriptor the kernel installed for us, so
                // taking ownership is correct and happens exactly once.
                unsafe {
                    let data = control.as_ptr().add(offset + CMSG_HDR_LEN).cast::<RawFd>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read()));
                    }
                }
            }
            let step = len.cmsg_aligned();
            if step == 0 {
                break;
            }
            offset += step;
        }
        fds
    }
}
