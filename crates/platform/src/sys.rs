#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! The thin layer of raw syscalls and libc FFI we cannot avoid.
//!
//! With io_uring (via `ququmatz`) carrying all socket I/O, what's left here is:
//!
//! * **the shared-memory pixel buffer** — `memfd_create` / `ftruncate` / `mmap`.
//!   `ququmatz` doesn't cover memfd, so this stays as direct libc FFI.
//! * **ancillary-data construction** — building the `SCM_RIGHTS` control buffer
//!   that rides on a `sendmsg` to hand the compositor our shm fd. The `sendmsg`
//!   itself is now an io_uring SQE; we only assemble the bytes here, pointed at
//!   `ququmatz::MsgHdr`.
//! * **a `sockaddr_un`** for connecting to the Wayland unix socket (`ququmatz`'s
//!   typed socket API is IPv4-only, so we build the address bytes ourselves).
//!
//! libc is always linked into a Rust binary, so this is "zero crates" but not
//! "zero FFI" — graphics requires the kernel.

use std::ffi::c_void;
use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};

use ququmatz::types::{IoVec, MsgHdr};

/// A failure in one of the raw syscalls this module makes. Each variant names
/// the syscall and carries the underlying OS error.
#[derive(Debug)]
pub enum SysError {
    /// `socket(2)` failed.
    Socket(io::Error),
    /// `memfd_create(2)` failed.
    Memfd(io::Error),
    /// `ftruncate(2)` failed sizing the shm pool.
    Ftruncate(io::Error),
    /// `mmap(2)` failed mapping the shm pool.
    Mmap(io::Error),
    /// `timerfd_create(2)` failed creating the key-repeat timer.
    TimerfdCreate(io::Error),
    /// `timerfd_settime(2)` failed (dis)arming the key-repeat timer.
    TimerfdSet(io::Error),
}

impl fmt::Display for SysError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SysError::Socket(e) => write!(f, "socket() failed: {e}"),
            SysError::Memfd(e) => write!(f, "memfd_create() failed: {e}"),
            SysError::Ftruncate(e) => write!(f, "ftruncate() failed: {e}"),
            SysError::Mmap(e) => write!(f, "mmap() failed: {e}"),
            SysError::TimerfdCreate(e) => write!(f, "timerfd_create() failed: {e}"),
            SysError::TimerfdSet(e) => write!(f, "timerfd_settime() failed: {e}"),
        }
    }
}

impl std::error::Error for SysError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SysError::Socket(e)
            | SysError::Memfd(e)
            | SysError::Ftruncate(e)
            | SysError::Mmap(e)
            | SysError::TimerfdCreate(e)
            | SysError::TimerfdSet(e) => Some(e),
        }
    }
}

// --- libc bindings -------------------------------------------------------
// We declare exactly the symbols we call. Signatures match <sys/mman.h> and
// <sys/memfd.h> on x86-64 Linux.

#[allow(non_camel_case_types)]
type size_t = usize;

const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const MAP_SHARED: i32 = 0x1;
const MAP_FAILED: *mut c_void = usize::MAX as *mut c_void;
/// `mremap` flag: allow the kernel to relocate the mapping if it can't grow it
/// in place. Without it a grow that can't extend the current VMA fails.
const MREMAP_MAYMOVE: i32 = 0x1;

unsafe extern "C" {
    fn memfd_create(name: *const u8, flags: u32) -> RawFd;
    fn ftruncate(fd: RawFd, length: i64) -> i32;
    fn mmap(
        addr: *mut c_void,
        length: size_t,
        prot: i32,
        flags: i32,
        fd: RawFd,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: size_t) -> i32;
    fn mremap(
        old_address: *mut c_void,
        old_size: size_t,
        new_size: size_t,
        flags: i32,
    ) -> *mut c_void;
    fn timerfd_create(clockid: i32, flags: i32) -> RawFd;
    fn timerfd_settime(
        fd: RawFd,
        flags: i32,
        new_value: *const Itimerspec,
        old_value: *mut Itimerspec,
    ) -> i32;
    fn read(fd: RawFd, buf: *mut c_void, count: size_t) -> isize;
}

// `timerfd_create` clock + the `struct itimerspec` it's (dis)armed with. A
// `CLOCK_MONOTONIC` timerfd becomes readable each time it expires; reading it
// yields a `u64` expiration count. We use it as the key-repeat clock: arm it
// with an initial delay + a repeat interval while a key is held, disarm on
// release. The reader thread watches the fd; the main thread owns the timing.
const CLOCK_MONOTONIC: i32 = 1;

/// `struct timespec` (POSIX), the half of an `itimerspec`. Distinct from
/// ququmatz's io_uring [`Timespec`](ququmatz::types::Timespec) — different ABI,
/// different syscall.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

/// `struct itimerspec`: `it_value` is the first expiry, `it_interval` the
/// period for every expiry after. A zero `it_value` disarms the timer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Itimerspec {
    it_interval: Timespec,
    it_value: Timespec,
}

impl Timespec {
    /// Split milliseconds into the seconds/nanoseconds a timespec wants.
    fn from_millis(ms: u64) -> Self {
        Timespec {
            tv_sec: (ms / 1000) as i64,
            tv_nsec: ((ms % 1000) * 1_000_000) as i64,
        }
    }
}

// --- ancillary-data (SCM_RIGHTS) assembly --------------------------------
// The cmsg layout is the same regardless of who issues the sendmsg. We build
// the control buffer and point a `ququmatz::MsgHdr` at it; the actual transmit
// is an io_uring SENDMSG SQE in `conn`.

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;

// `struct cmsghdr` — the header of one ancillary-data item.
#[repr(C)]
struct CmsgHdr {
    cmsg_len: size_t,
    cmsg_level: i32,
    cmsg_type: i32,
}

const CMSG_HDR_LEN: usize = std::mem::size_of::<CmsgHdr>();

/// Extension trait rounding a byte length up to the platform word
/// alignment, matching the kernel's `CMSG_ALIGN`.
///
/// Was a free `cmsg_align(len: usize)` function -- its sole argument is a
/// bare `usize`, so it reads as a method on that value
/// (`len.cmsg_aligned()`), matching the `RawSyscallReturn`-on-`isize` style
/// used in `pty::sys` for the same kind of "raw primitive, no natural
/// receiver type" helper.
trait CmsgAlign {
    fn cmsg_aligned(self) -> Self;
}

impl CmsgAlign for usize {
    fn cmsg_aligned(self) -> Self {
        let a = std::mem::size_of::<size_t>();
        (self + a - 1) & !(a - 1)
    }
}

/// A `sendmsg`/`recvmsg` message: a data buffer plus an optional SCM_RIGHTS
/// control buffer carrying file descriptors. Owns its `iovec` and control
/// bytes so the pointers in the [`MsgHdr`] it produces stay valid as long as
/// `self` is alive — the caller must keep it alive until the io_uring SENDMSG
/// completes.
pub struct OutgoingMessage {
    iov: IoVec,
    control: Vec<u8>,
}

impl OutgoingMessage {
    /// Build a message that sends `bytes`, attaching `fds` as SCM_RIGHTS
    /// ancillary data. `bytes` must outlive the message (we borrow it into the
    /// iovec).
    pub fn new(bytes: &[u8], fds: &[BorrowedFd<'_>]) -> Self {
        // SAFETY: `bytes` outlives the message per the contract above; IoVec
        // borrows the pointer/len for the duration of the send.
        let iov = unsafe { IoVec::new(bytes.as_ptr() as *mut u8, bytes.len()) };

        let control = if fds.is_empty() {
            Vec::new()
        } else {
            let fd_bytes = std::mem::size_of::<RawFd>() * fds.len();
            let total = (CMSG_HDR_LEN + fd_bytes).cmsg_aligned();
            let mut buf = vec![0u8; total];
            // SAFETY: `buf` is sized for the header + fd payload computed above;
            // we write exactly that layout.
            unsafe {
                let cmsg = buf.as_mut_ptr() as *mut CmsgHdr;
                (*cmsg).cmsg_len = CMSG_HDR_LEN + fd_bytes;
                (*cmsg).cmsg_level = SOL_SOCKET;
                (*cmsg).cmsg_type = SCM_RIGHTS;
                let data = buf.as_mut_ptr().add(CMSG_HDR_LEN) as *mut RawFd;
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
        hdr.msg_iov = &mut self.iov;
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
}

/// The received counterpart to [`OutgoingMessage`]: reading SCM_RIGHTS
/// ancillary data back out of a control (cmsg) region the kernel wrote.
///
/// A zero-sized namespace rather than free functions at module scope -- same
/// reasoning as `pty::sys::RawPty`: there's no fd/message value in hand yet
/// to attach these to (they parse one), so the idiomatic home is an
/// associated function on a type that names the concept, not a bare
/// module-level fn.
pub struct IncomingControl;

impl IncomingControl {
    /// The fixed control-region width to reserve on every multishot recvmsg,
    /// sized to hold one SCM_RIGHTS cmsg carrying up to `max_fds`
    /// descriptors. This is the `msg_controllen` set on the persistent recv
    /// `MsgHdr` -- and thus the width of the control region inside each
    /// provided buffer.
    pub fn reserve_len(max_fds: usize) -> usize {
        (CMSG_HDR_LEN + std::mem::size_of::<RawFd>() * max_fds).cmsg_aligned()
    }

    /// Parse SCM_RIGHTS file descriptors out of a received control (cmsg)
    /// region, taking ownership of each. `control` is the control slice the
    /// kernel wrote -- e.g. `RecvmsgParts::control` from a multishot recvmsg
    /// buffer.
    pub fn fds(control: &[u8]) -> Vec<OwnedFd> {
        use std::os::fd::FromRawFd;
        let mut fds = Vec::new();
        if control.len() >= CMSG_HDR_LEN {
            // SAFETY: a control region of at least CMSG_HDR_LEN bytes holds a
            // valid cmsghdr; we read its fields and, if SCM_RIGHTS, the fd
            // payload behind it (bounded by cmsg_len, which the kernel set).
            unsafe {
                let cmsg = control.as_ptr() as *const CmsgHdr;
                if (*cmsg).cmsg_level == SOL_SOCKET
                    && (*cmsg).cmsg_type == SCM_RIGHTS
                    && (*cmsg).cmsg_len >= CMSG_HDR_LEN
                    && (*cmsg).cmsg_len <= control.len()
                {
                    let payload = (*cmsg).cmsg_len - CMSG_HDR_LEN;
                    let count = payload / std::mem::size_of::<RawFd>();
                    let data = control.as_ptr().add(CMSG_HDR_LEN) as *const RawFd;
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read()));
                    }
                }
            }
        }
        fds
    }
}

// --- AF_UNIX address ------------------------------------------------------

/// `AF_UNIX` (Linux constant).
pub const AF_UNIX: i32 = 1;

/// A `struct sockaddr_un` built for `connect(2)`: `sa_family` (u16,
/// host-endian) followed by the NUL-terminated path in `sun_path` (108
/// bytes).
///
/// A real value type (built once, read via [`SockAddrUn::as_bytes`]) rather
/// than a `(Vec<u8>, u32)` tuple handed back from a free function -- the
/// pairing of "these bytes" with "this many of them are valid" is exactly
/// what a struct communicates and a tuple doesn't.
pub struct SockAddrUn {
    buf: Vec<u8>,
    len: u32,
}

impl SockAddrUn {
    const SUN_PATH_LEN: usize = 108;
    const FAMILY_LEN: usize = 2;

    /// Build the address for `path`.
    ///
    /// The valid length is `offsetof(sun_path) + strlen(path) + 1` (the NUL
    /// terminator), which is what `connect` expects for a pathname socket.
    pub fn new(path: &[u8]) -> Self {
        let mut buf = vec![0u8; Self::FAMILY_LEN + Self::SUN_PATH_LEN];
        buf[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
        let n = path.len().min(Self::SUN_PATH_LEN - 1);
        buf[Self::FAMILY_LEN..Self::FAMILY_LEN + n].copy_from_slice(&path[..n]);
        // NUL terminator is already there (buf zero-initialised).
        let len = (Self::FAMILY_LEN + n + 1) as u32;
        Self { buf, len }
    }

    /// The valid `sockaddr_un` bytes (i.e. `buf[..len]`), ready for
    /// `connect(2)`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len as usize]
    }
}

// --- shared-memory pixel buffer ------------------------------------------

/// A shared-memory pool holding one or more equally-sized frames the compositor
/// reads from. We draw into one frame while the compositor displays another
/// (double-buffering), so it never samples a half-drawn image. All frames live
/// in a single mapping; `frame` indices select between them.
pub struct ShmBuffer {
    ptr: *mut u32,
    /// Pixels in one frame (`width * height`).
    frame_pixels: usize,
    /// Bytes in one frame (`frame_pixels * 4`).
    frame_bytes: usize,
    frames: usize,
    /// Bytes actually backing the mapping / fd / server-side pool. Grows only
    /// (never shrinks) so we can reuse the same `wl_shm_pool` across resizes:
    /// on a shrink the logical `frame_bytes` drops but this stays put, so the
    /// mapping and pool are left oversized and the extra space sits idle until
    /// a later grow reclaims it. Always `>= frame_bytes * frames`.
    capacity_bytes: usize,
    fd: OwnedFd,
}

impl ShmBuffer {
    /// Allocate `frames` back-to-back `width * height` ARGB frames in one
    /// anonymous, page-backed mapping shared via the returned fd.
    pub fn new(width: usize, height: usize, frames: usize) -> Result<Self, SysError> {
        use std::os::fd::FromRawFd;

        assert!(frames >= 1);
        let frame_pixels = width * height;
        let frame_bytes = frame_pixels * 4;
        let total_bytes = frame_bytes * frames;

        // memfd: an anonymous in-RAM file. Name is purely for debugging.
        let name = b"paint-shm\0";
        // `MFD_CLOEXEC` (1): don't leak the shm fd into PTY-shell children. A
        // child that inherits it keeps the pool alive; combined with a leaked
        // Wayland socket that's what freezes the window after we exit.
        const MFD_CLOEXEC: u32 = 0x0001;
        // SAFETY: name is a valid NUL-terminated C string.
        let raw = unsafe { memfd_create(name.as_ptr(), MFD_CLOEXEC) };
        if raw < 0 {
            return Err(SysError::Memfd(io::Error::last_os_error()));
        }
        // SAFETY: memfd_create returned a fresh, owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // Size the file to hold every frame.
        // SAFETY: fd is valid; length is non-negative.
        if unsafe { ftruncate(fd.as_raw_fd(), total_bytes as i64) } < 0 {
            return Err(SysError::Ftruncate(io::Error::last_os_error()));
        }

        // Map the whole pool writable and shared.
        // SAFETY: fd is valid and sized to total_bytes; offset 0.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                total_bytes,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == MAP_FAILED {
            return Err(SysError::Mmap(io::Error::last_os_error()));
        }

        Ok(ShmBuffer {
            ptr: ptr as *mut u32,
            frame_pixels,
            frame_bytes,
            frames,
            capacity_bytes: total_bytes,
            fd,
        })
    }

    /// Number of frames in the pool.
    #[allow(dead_code)] // part of the buffer abstraction; used once N>2 frames.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Frame `frame`'s pixels, as a writable slice of XRGB8888 values.
    pub fn pixels(&mut self, frame: usize) -> &mut [u32] {
        assert!(frame < self.frames);
        // SAFETY: each frame occupies a disjoint `frame_pixels` window of the
        // mapping; `&mut self` makes this the only live borrow.
        unsafe {
            let base = self.ptr.add(frame * self.frame_pixels);
            std::slice::from_raw_parts_mut(base, self.frame_pixels)
        }
    }

    /// Byte offset of frame `frame` within the pool (for `create_buffer`).
    pub fn frame_offset(&self, frame: usize) -> usize {
        frame * self.frame_bytes
    }

    /// Borrow the underlying fd to hand to `wl_shm.create_pool`.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Duplicate the underlying fd for sending through the MPSC ring — the
    /// original must stay open for the `mmap` mapping. Kept for callers that
    /// hand a fresh pool fd to the compositor; `resize` no longer re-sends the
    /// fd (it grows the existing pool in place).
    #[allow(dead_code)]
    pub fn dup_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Total pool size in bytes — the `create_pool` / `wl_shm_pool.resize`
    /// size. This is the *capacity* (grow-only), which after a lazy shrink is
    /// larger than the `frame_bytes * frames` currently in use.
    pub fn size_bytes(&self) -> usize {
        self.capacity_bytes
    }

    /// Re-shape the pool to hold `frames` frames of `width * height`, reusing
    /// the existing mapping and fd instead of allocating a fresh pool.
    ///
    /// Returns `Some(new_capacity)` when the backing store had to *grow* — the
    /// fd was `ftruncate`d up and the mapping `mremap`ped, so the caller must
    /// issue a `wl_shm_pool.resize` to `new_capacity` before creating buffers
    /// past the old end. Returns `None` when the new layout fit inside the
    /// current capacity: the frame dimensions shrank (or held), the mapping is
    /// untouched, and no server round-trip is needed (lazy shrink).
    ///
    /// Growth is generous — the capacity at least doubles — so a drag's stream
    /// of resizes settles into `None` (no syscalls, no re-`mmap`) after the
    /// first couple of steps.
    pub fn resize(&mut self, width: usize, height: usize) -> Result<Option<usize>, SysError> {
        let frame_pixels = width * height;
        let frame_bytes = frame_pixels * 4;
        let needed = frame_bytes * self.frames;
        self.frame_pixels = frame_pixels;
        self.frame_bytes = frame_bytes;

        if needed <= self.capacity_bytes {
            // Fits — keep the oversized mapping, skip the server round-trip.
            return Ok(None);
        }

        // Grow-only, doubling so repeated small grows amortise to nothing.
        let new_cap = needed.max(self.capacity_bytes.saturating_mul(2));

        // SAFETY: fd is our own memfd; growing a memfd never truncates data.
        if unsafe { ftruncate(self.fd.as_raw_fd(), new_cap as i64) } < 0 {
            return Err(SysError::Ftruncate(io::Error::last_os_error()));
        }
        // SAFETY: `ptr`/`capacity_bytes` describe our live mapping; MAYMOVE lets
        // the kernel relocate it if it can't extend in place.
        let ptr = unsafe {
            mremap(
                self.ptr as *mut c_void,
                self.capacity_bytes,
                new_cap,
                MREMAP_MAYMOVE,
            )
        };
        if ptr == MAP_FAILED {
            return Err(SysError::Mmap(io::Error::last_os_error()));
        }
        self.ptr = ptr as *mut u32;
        self.capacity_bytes = new_cap;
        Ok(Some(new_cap))
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        // SAFETY: ptr/len came from a successful mmap and are unmapped once.
        unsafe {
            munmap(self.ptr as *mut c_void, self.size_bytes());
        }
    }
}

// --- key-repeat timer (timerfd) ------------------------------------------

/// A `CLOCK_MONOTONIC` timerfd used as the key-repeat clock.
///
/// The compositor sends key *transitions* (`key` press/release), never repeats —
/// synthesising repeats is the client's job, parameterised by the `repeat_info`
/// event's rate and delay. We drive that off a timerfd: when a movement key goes
/// down we [`arm`](Self::arm) it (first expiry after `delay`, then every
/// `interval`); on release we [`disarm`](Self::disarm) it.
///
/// The fd is owned here but watched by the reader thread, which polls it on its
/// io_uring ring and turns each expiry into an event. Ownership and timing stay
/// on the main thread; the reader only translates readiness into an [`Event`].
pub struct RepeatTimer {
    fd: OwnedFd,
}

impl RepeatTimer {
    /// Create a disarmed `CLOCK_MONOTONIC` timerfd.
    pub fn new() -> Result<Self, SysError> {
        use std::os::fd::FromRawFd;
        // `TFD_CLOEXEC` (02000000): don't leak the key-repeat timerfd into
        // PTY-shell children. Same close-on-exec hygiene as the shm memfd and
        // the Wayland socket.
        const TFD_CLOEXEC: i32 = 0o2_000_000;
        // SAFETY: timerfd_create with a valid clock + CLOEXEC; we adopt the fd.
        let raw = unsafe { timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC) };
        if raw < 0 {
            return Err(SysError::TimerfdCreate(io::Error::last_os_error()));
        }
        // SAFETY: timerfd_create returned a fresh, owned fd.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(RepeatTimer { fd })
    }

    /// Arm the timer: first expiry after `delay_ms`, then once every
    /// `interval_ms`. Re-arming replaces any previous schedule.
    pub fn arm(&self, delay_ms: u64, interval_ms: u64) -> Result<(), SysError> {
        let spec = Itimerspec {
            it_value: Timespec::from_millis(delay_ms),
            it_interval: Timespec::from_millis(interval_ms),
        };
        self.set(&spec)
    }

    /// Disarm the timer (a zero `it_value` stops it). No-op if already disarmed.
    pub fn disarm(&self) -> Result<(), SysError> {
        self.set(&Itimerspec::default())
    }

    fn set(&self, spec: &Itimerspec) -> Result<(), SysError> {
        // SAFETY: fd is a valid timerfd; `spec` is a valid itimerspec we own;
        // passing null for old_value (we don't want the previous setting).
        let rc = unsafe { timerfd_settime(self.fd.as_raw_fd(), 0, spec, std::ptr::null_mut()) };
        if rc < 0 {
            return Err(SysError::TimerfdSet(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Borrow the underlying fd for the reader thread to poll.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Drain a timerfd's pending expiration count, clearing its
    /// level-triggered readiness so a re-armed poll won't immediately
    /// re-fire. The count itself is discarded -- one readiness edge should
    /// yield one repeat regardless of how many expiries piled up (we don't
    /// want to fast-forward the box if the loop stalled briefly).
    ///
    /// An associated function taking a bare `fd`, not a `&self` method: the
    /// reader thread polls only the raw integer (it never owns the
    /// [`RepeatTimer`], which stays on the main thread), so there's no
    /// `Self` to call this on -- same split as the socket fd, which is
    /// likewise shared as a raw handle across the two threads.
    pub fn drain_fd(fd: RawFd) {
        let mut buf = [0u8; 8];
        // SAFETY: reading up to 8 bytes into a local 8-byte buffer from a
        // valid fd. A timerfd read yields exactly a u64 count or EAGAIN; we
        // ignore both.
        unsafe {
            read(fd, buf.as_mut_ptr() as *mut c_void, buf.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    fn io_err(msg: &str) -> io::Error {
        io::Error::other(msg.to_string())
    }

    #[test]
    fn sys_error_display_messages() {
        assert_eq!(
            SysError::Socket(io_err("x")).to_string(),
            "socket() failed: x"
        );
        assert_eq!(
            SysError::Memfd(io_err("x")).to_string(),
            "memfd_create() failed: x"
        );
        assert_eq!(
            SysError::Ftruncate(io_err("x")).to_string(),
            "ftruncate() failed: x"
        );
        assert_eq!(SysError::Mmap(io_err("x")).to_string(), "mmap() failed: x");
        assert_eq!(
            SysError::TimerfdCreate(io_err("x")).to_string(),
            "timerfd_create() failed: x"
        );
        assert_eq!(
            SysError::TimerfdSet(io_err("x")).to_string(),
            "timerfd_settime() failed: x"
        );
    }

    #[test]
    fn sys_error_source_is_always_the_inner_io_error() {
        use std::error::Error as _;
        let e = SysError::Socket(io_err("boom"));
        assert!(e.source().is_some());
        assert_eq!(e.source().unwrap().to_string(), "boom");
    }

    #[test]
    fn incoming_control_fds_empty_control_region_yields_no_fds() {
        assert!(IncomingControl::fds(&[]).is_empty());
        assert!(IncomingControl::fds(&[0u8; 4]).is_empty());
    }

    #[test]
    fn incoming_control_fds_roundtrips_through_outgoing_message() {
        // Build a real SCM_RIGHTS control buffer via `OutgoingMessage` (the
        // send side) using stdin's fd (always valid in a test process), then
        // parse it back with `IncomingControl::fds` (the receive side) and
        // confirm we get exactly one fd out.
        let stdin = std::io::stdin();
        let fd = stdin.as_fd();
        let mut msg = OutgoingMessage::new(b"hello", &[fd]);
        let hdr = msg.header();
        assert!(!hdr.msg_control.is_null());
        // Reconstruct the control region as a byte slice the way a received
        // cmsg buffer would look.
        let control =
            unsafe { std::slice::from_raw_parts(hdr.msg_control as *const u8, hdr.msg_controllen) };
        let fds = IncomingControl::fds(control);
        assert_eq!(fds.len(), 1);
    }

    #[test]
    fn incoming_control_fds_reserve_len_grows_with_max_fds() {
        let one = IncomingControl::reserve_len(1);
        let many = IncomingControl::reserve_len(16);
        // Alignment can make adjacent fd counts collide on the same rounded
        // word boundary, so compare a small count against a much larger one
        // to guarantee a real difference.
        assert!(many > one);
        // Always at least big enough for the cmsg header itself.
        assert!(one >= CMSG_HDR_LEN);
        assert_eq!(IncomingControl::reserve_len(0), CMSG_HDR_LEN.cmsg_aligned());
    }

    #[test]
    fn incoming_control_fds_rejects_truncated_cmsg_len() {
        // A header claiming a cmsg_len larger than the actual control slice
        // must be treated as absent, not read out of bounds.
        let mut buf = vec![0u8; CMSG_HDR_LEN];
        unsafe {
            let cmsg = buf.as_mut_ptr() as *mut CmsgHdr;
            (*cmsg).cmsg_len = 9999; // way past buf.len()
            (*cmsg).cmsg_level = SOL_SOCKET;
            (*cmsg).cmsg_type = SCM_RIGHTS;
        }
        assert!(IncomingControl::fds(&buf).is_empty());
    }

    #[test]
    fn incoming_control_fds_ignores_non_scm_rights_cmsg() {
        let mut buf = vec![0u8; CMSG_HDR_LEN + 8];
        unsafe {
            let cmsg = buf.as_mut_ptr() as *mut CmsgHdr;
            (*cmsg).cmsg_len = CMSG_HDR_LEN + 8;
            (*cmsg).cmsg_level = SOL_SOCKET;
            (*cmsg).cmsg_type = 99; // not SCM_RIGHTS
        }
        assert!(IncomingControl::fds(&buf).is_empty());
    }

    #[test]
    fn shm_resize_grows_only_when_needed_and_shrinks_lazily() {
        // Start small, then walk the size around to exercise grow + lazy shrink.
        let mut shm = ShmBuffer::new(64, 64, 2).expect("initial alloc");
        let cap0 = shm.size_bytes();
        assert_eq!(cap0, 64 * 64 * 4 * 2);

        // Growing past capacity re-truncates + remaps and reports the new size,
        // which at least doubles (generous, amortised) growth.
        let grew = shm.resize(200, 200).expect("grow");
        let cap1 = grew.expect("grow should report a new capacity");
        assert_eq!(cap1, shm.size_bytes());
        assert!(cap1 >= 200 * 200 * 4 * 2);
        assert!(cap1 >= cap0 * 2);

        // The mapping must be writable across the full new frame at both slots.
        shm.pixels(0)[200 * 200 - 1] = 0xdead_beef;
        shm.pixels(1)[0] = 0x00c0_ffee;
        assert_eq!(shm.pixels(0)[200 * 200 - 1], 0xdead_beef);

        // Shrinking keeps the oversized pool: no re-map, no server round-trip.
        assert!(shm.resize(100, 100).expect("shrink").is_none());
        assert_eq!(shm.size_bytes(), cap1);
        // frame_offset now reflects the smaller logical frame, still in-bounds.
        assert_eq!(shm.frame_offset(1), 100 * 100 * 4);

        // Growing again but still within the retained capacity stays lazy.
        assert!(shm.resize(150, 150).expect("regrow within cap").is_none());
        assert_eq!(shm.size_bytes(), cap1);
    }

    #[test]
    fn cmsg_align_rounds_up_to_word_size() {
        let word = std::mem::size_of::<size_t>();
        assert_eq!(0usize.cmsg_aligned(), 0);
        assert_eq!(1usize.cmsg_aligned(), word);
        assert_eq!(word.cmsg_aligned(), word);
        assert_eq!((word + 1).cmsg_aligned(), word * 2);
    }
}

use std::os::fd::AsFd;
