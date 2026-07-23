//! The sliver of raw libc this crate can't avoid: making the `AF_UNIX`
//! socket and building its `sockaddr_un`, plus reading our own uid for SASL
//! `EXTERNAL` auth.
//!
//! Mirrors `platform::sys`'s `SockAddrUn` (that crate isn't a dependency
//! here, so this is a small, deliberate duplication rather than reaching
//! across crates for a dozen lines).

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[allow(non_camel_case_types)]
type RawFdInt = i32;

unsafe extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> RawFdInt;
    fn geteuid() -> u32;
    fn getpid() -> i32;
}

pub const AF_UNIX: i32 = 1;
const SOCK_STREAM: i32 = 1;
/// Don't leak the bus connection into subprocesses the statusbar's other
/// modules spawn (`pactl`, `iw`, …) — same close-on-exec hygiene
/// `platform::sys` applies to the Wayland socket.
const SOCK_CLOEXEC: i32 = 0o2_000_000;

/// The sliver of raw libc syscalls this crate can't avoid.
///
/// A zero-sized marker type (not a value with fields) grouping
/// `socket(2)`/`geteuid(2)`/`getpid(2)` wrappers under one owner instead of
/// leaving them as free functions, the same `RawPty`-style pattern
/// `crate::pty` uses for its own unavoidable libc sliver.
pub struct DbusSys;

impl DbusSys {
    /// Create an `AF_UNIX`, `SOCK_STREAM`, close-on-exec socket.
    pub fn make_unix_socket() -> io::Result<OwnedFd> {
        // SAFETY: standard socket(2) call with fixed, valid arguments.
        let fd = unsafe { socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socket() returned a fresh owned fd.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// The effective uid of this process, as used by SASL `EXTERNAL` auth.
    ///
    /// The mechanism proves identity via `SO_PEERCRED`, but still sends the
    /// uid as an ASCII-decimal, hex-encoded string in the `AUTH` line.
    #[must_use]
    pub fn effective_uid() -> u32 {
        // SAFETY: geteuid takes no arguments and cannot fail.
        unsafe { geteuid() }
    }

    /// This process's own pid, used to ask logind (`GetSessionByPID`) which
    /// session we belong to without needing `XDG_SESSION_ID` to be set.
    #[must_use]
    pub fn own_pid() -> u32 {
        // SAFETY: getpid takes no arguments and cannot fail.
        let pid = unsafe { getpid() };
        // A pid is always non-negative in practice (getpid(2) never returns
        // a negative value); fall back to 0 -- a pid `logind` will never
        // recognize as any real session -- rather than panic if that were
        // ever somehow violated.
        u32::try_from(pid).unwrap_or(0)
    }

    /// Borrow a socket's raw fd as `ququmatz`'s
    /// [`RawFd`](ququmatz::types::RawFd).
    #[must_use]
    pub fn as_uring_fd(fd: &OwnedFd) -> ququmatz::types::RawFd {
        // A raw fd is always non-negative; usize::try_from only fails on a
        // negative fd, which as_raw_fd() never returns for an OwnedFd.
        ququmatz::types::RawFd::from_raw(usize::try_from(fd.as_raw_fd()).unwrap_or(0))
    }
}

/// A `struct sockaddr_un` built for `connect(2)`. Same layout/contract as
/// `platform::sys::SockAddrUn` — see that type's docs.
pub struct SockAddrUn {
    buf: Vec<u8>,
    len: u32,
}

impl SockAddrUn {
    const SUN_PATH_LEN: usize = 108;
    const FAMILY_LEN: usize = 2;

    /// Build the address for a pathname socket at `path`.
    #[must_use]
    pub fn new(path: &[u8]) -> Self {
        let mut buf = vec![0u8; Self::FAMILY_LEN + Self::SUN_PATH_LEN];
        buf[0..2].copy_from_slice(&u16::try_from(AF_UNIX).unwrap_or(0).to_ne_bytes());
        let n = path.len().min(Self::SUN_PATH_LEN - 1);
        buf[Self::FAMILY_LEN..Self::FAMILY_LEN + n].copy_from_slice(&path[..n]);
        let len = u32::try_from(Self::FAMILY_LEN + n + 1).unwrap_or(u32::MAX);
        Self { buf, len }
    }

    /// Build the address for a Linux abstract-namespace socket: `name`
    /// becomes `sun_path[0] = 0` followed by `name`'s bytes, no NUL
    /// terminator (abstract sockets are length-addressed, not
    /// NUL-terminated) — used for `DBUS_SESSION_BUS_ADDRESS` entries of the
    /// form `unix:abstract=/tmp/dbus-xyz`.
    #[must_use]
    pub fn new_abstract(name: &[u8]) -> Self {
        let mut buf = vec![0u8; Self::FAMILY_LEN + Self::SUN_PATH_LEN];
        buf[0..2].copy_from_slice(&u16::try_from(AF_UNIX).unwrap_or(0).to_ne_bytes());
        // buf[FAMILY_LEN] is already 0 (the abstract marker byte).
        let n = name.len().min(Self::SUN_PATH_LEN - 1);
        buf[Self::FAMILY_LEN + 1..Self::FAMILY_LEN + 1 + n].copy_from_slice(&name[..n]);
        let len = u32::try_from(Self::FAMILY_LEN + 1 + n).unwrap_or(u32::MAX);
        Self { buf, len }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockaddr_pathname_layout() {
        let addr = SockAddrUn::new(b"/var/run/dbus/system_bus_socket");
        let bytes = addr.as_bytes();
        assert_eq!(
            &bytes[0..2],
            &u16::try_from(AF_UNIX).unwrap_or(0).to_ne_bytes()
        );
        let path = b"/var/run/dbus/system_bus_socket";
        assert_eq!(&bytes[2..2 + path.len()], path);
        assert_eq!(bytes[2 + path.len()], 0); // NUL terminator present
    }

    #[test]
    fn sockaddr_abstract_layout_has_no_nul_terminator_requirement() {
        let addr = SockAddrUn::new_abstract(b"/tmp/dbus-abcd");
        let bytes = addr.as_bytes();
        assert_eq!(bytes[2], 0); // abstract marker
        assert_eq!(&bytes[3..3 + 14], b"/tmp/dbus-abcd");
        assert_eq!(bytes.len(), 2 + 1 + 14);
    }

    #[test]
    fn effective_uid_is_plausible() {
        // Just confirm it doesn't panic and returns something -- can't assert
        // a specific value in a sandboxed test runner.
        let _ = DbusSys::effective_uid();
    }
}
