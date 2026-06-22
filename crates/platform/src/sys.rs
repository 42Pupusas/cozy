#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
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

/// Round `len` up to the platform word alignment, matching the kernel's
/// `CMSG_ALIGN`.
const fn cmsg_align(len: usize) -> usize {
    let a = std::mem::size_of::<size_t>();
    (len + a - 1) & !(a - 1)
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
            let total = cmsg_align(CMSG_HDR_LEN + fd_bytes);
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

/// The fixed control-region width we reserve on every multishot recvmsg, sized
/// to hold one SCM_RIGHTS cmsg carrying up to `max_fds` descriptors. This is the
/// `msg_controllen` we set on the persistent recv `MsgHdr` — and thus the width
/// of the control region inside each provided buffer.
pub fn recv_control_len(max_fds: usize) -> usize {
    cmsg_align(CMSG_HDR_LEN + std::mem::size_of::<RawFd>() * max_fds)
}

/// Parse SCM_RIGHTS file descriptors out of a received control (cmsg) region,
/// taking ownership of each. `control` is the control slice the kernel wrote —
/// e.g. `RecvmsgParts::control` from a multishot recvmsg buffer.
pub fn fds_from_control(control: &[u8]) -> Vec<OwnedFd> {
    use std::os::fd::FromRawFd;
    let mut fds = Vec::new();
    if control.len() >= CMSG_HDR_LEN {
        // SAFETY: a control region of at least CMSG_HDR_LEN bytes holds a valid
        // cmsghdr; we read its fields and, if SCM_RIGHTS, the fd payload behind
        // it (bounded by cmsg_len, which the kernel set).
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

// --- AF_UNIX address ------------------------------------------------------

/// `AF_UNIX` (Linux constant).
pub const AF_UNIX: i32 = 1;

/// Build the bytes of a `struct sockaddr_un` for `path`, plus its valid length.
///
/// Layout: `sa_family` (u16, host-endian) followed by the NUL-terminated path
/// in `sun_path` (108 bytes). The returned length is `offsetof(sun_path) +
/// strlen(path) + 1`, which is what `connect` expects for a pathname socket.
pub fn sockaddr_un(path: &[u8]) -> (Vec<u8>, u32) {
    const SUN_PATH_LEN: usize = 108;
    const FAMILY_LEN: usize = 2;
    let mut buf = vec![0u8; FAMILY_LEN + SUN_PATH_LEN];
    buf[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
    let n = path.len().min(SUN_PATH_LEN - 1);
    buf[FAMILY_LEN..FAMILY_LEN + n].copy_from_slice(&path[..n]);
    // NUL terminator is already there (buf zero-initialised).
    let addr_len = (FAMILY_LEN + n + 1) as u32;
    (buf, addr_len)
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
        // SAFETY: name is a valid NUL-terminated C string; flags = 0.
        let raw = unsafe { memfd_create(name.as_ptr(), 0) };
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

    /// Duplicate the underlying fd for sending through the MPSC ring during
    /// resize — the original must stay open for the `mmap` mapping.
    pub fn dup_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// Total pool size in bytes (all frames) — the `create_pool` size.
    pub fn size_bytes(&self) -> usize {
        self.frame_bytes * self.frames
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
        // SAFETY: a plain timerfd_create with no flags; we adopt the owned fd.
        let raw = unsafe { timerfd_create(CLOCK_MONOTONIC, 0) };
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
}

/// Drain a timerfd's pending expiration count, clearing its level-triggered
/// readiness so a re-armed poll won't immediately re-fire. The count itself is
/// discarded — one readiness edge should yield one repeat regardless of how many
/// expiries piled up (we don't want to fast-forward the box if the loop stalled
/// briefly). Called from the reader thread, which polls the fd; it borrows only
/// the raw integer, never the owning [`RepeatTimer`].
pub fn drain_timerfd(fd: RawFd) {
    let mut buf = [0u8; 8];
    // SAFETY: reading up to 8 bytes into a local 8-byte buffer from a valid fd.
    // A timerfd read yields exactly a u64 count or EAGAIN; we ignore both.
    unsafe {
        read(fd, buf.as_mut_ptr() as *mut c_void, buf.len());
    }
}

use std::os::fd::AsFd;
