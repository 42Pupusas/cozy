//! A connected `AF_UNIX` socket pair, for exercising descriptor passing
//! without a bus daemon.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use crate::sys::AF_UNIX;

unsafe extern "C" {
    fn socketpair(domain: i32, ty: i32, protocol: i32, sv: *mut RawFd) -> i32;
}

const SOCK_STREAM: i32 = 1;
const SOCK_CLOEXEC: i32 = 0o2_000_000;

/// Two connected `AF_UNIX` stream sockets.
///
/// `SCM_RIGHTS` needs a real unix socket with a real peer, so the transport
/// tests use this instead of a mock. It is the same socket family and type
/// the session bus uses, which is what makes those tests meaningful.
pub struct SocketPair {
    pub left: OwnedFd,
    pub right: OwnedFd,
}

impl SocketPair {
    /// Create a connected, close-on-exec stream pair.
    pub fn new() -> io::Result<Self> {
        let mut fds: [RawFd; 2] = [-1, -1];
        // SAFETY: `fds` is a two-element array, exactly what socketpair
        // writes into for a valid domain/type pair.
        let rc = unsafe { socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socketpair returned two fresh owned descriptors.
        unsafe {
            Ok(Self {
                left: OwnedFd::from_raw_fd(fds[0]),
                right: OwnedFd::from_raw_fd(fds[1]),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn a_pair_is_two_distinct_open_descriptors() {
        let pair = SocketPair::new().expect("socketpair");
        assert!(pair.left.as_raw_fd() >= 0);
        assert!(pair.right.as_raw_fd() >= 0);
        assert_ne!(pair.left.as_raw_fd(), pair.right.as_raw_fd());
    }
}
