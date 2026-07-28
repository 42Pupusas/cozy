#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! The Wayland connection, split across two threads by direction.
//!
//! All I/O to the compositor goes through `io_uring` (via `ququmatz`), over a
//! single `AF_UNIX` socket. To overlap input handling with rendering we use
//! **two independent `io_uring` rings on the same socket fd**:
//!
//! * [`Reader`] owns the receive ring and the provided-buffer pool. It runs on
//!   its own thread: arm a multishot `recvmsg`, block, reap completions, split
//!   the bytes into whole Wayland messages, and push each as a [`RawEvent`] onto a
//!   lock-free SPSC ring. Arm / reap / re-arm are all local to this ring, so the
//!   re-arm never crosses a thread boundary — no shared state.
//! * [`Sender`] owns the send ring and stays on the main thread. It allocates
//!   object ids and submits requests (`sendmsg`, optionally with `SCM_RIGHTS`
//!   fds). Sends and receives are independent directions on the stream socket,
//!   so the two rings never contend.
//!
//! [`connect`] does the socket setup and hands back both halves.

use std::fmt;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::PathBuf;

use quetzalcoatl::mpsc;
use quetzalcoatl::spsc::Producer;
use ququmatz::types::{CqeFlags, MsgFlags, MsgHdr, PollMask, RawFd};
use ququmatz::{IoUring, ProvidedBufferRing, Sqe};

use crate::event::RawEvent;
use crate::id::ObjectId;
use crate::progress_guard::{Progress, ProgressGuard};
use crate::sys::{self, OutgoingMessage, RepeatTimer, SysError};
use crate::wire::{MessageBuilder, WireEvent};

/// A failure setting up or running the Wayland connection.
#[derive(Debug)]
pub enum ConnError {
    /// A required environment variable was missing.
    Env(&'static str),
    /// `io_uring` ring setup (`io_uring_setup` + mmaps) failed.
    Setup(ququmatz::Error),
    /// Registering the provided-buffer pool failed.
    Register(ququmatz::Error),
    /// Pushing/submitting/waiting on the ring failed.
    Submit(ququmatz::Error),
    /// `connect(2)` to the compositor socket failed (errno from the CQE).
    Connect(io::Error),
    /// The kernel returned no completion after a wait that should have produced
    /// one.
    NoCompletion,
    /// A `recvmsg`/`sendmsg` op completed with a negative errno.
    Io(io::Error),
    /// The compositor closed the connection.
    Closed,
    /// A syscall in the `sys` layer failed.
    Sys(SysError),
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env(var) => write!(f, "environment variable {var} is not set"),
            Self::Setup(e) => write!(f, "io_uring setup failed: {e}"),
            Self::Register(e) => write!(f, "provided-buffer registration failed: {e}"),
            Self::Submit(e) => write!(f, "io_uring submit/wait failed: {e}"),
            Self::Connect(e) => write!(f, "connect to compositor failed: {e}"),
            Self::NoCompletion => write!(f, "io_uring returned no completion after wait"),
            Self::Io(e) => write!(f, "socket I/O failed: {e}"),
            Self::Closed => write!(f, "the compositor closed the connection"),
            Self::Sys(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup(e) | Self::Register(e) | Self::Submit(e) => Some(e),
            Self::Connect(e) | Self::Io(e) => Some(e),
            Self::Sys(e) => Some(e),
            Self::Env(_) | Self::NoCompletion | Self::Closed => None,
        }
    }
}

impl From<SysError> for ConnError {
    fn from(e: SysError) -> Self {
        Self::Sys(e)
    }
}

/// Extends [`IoUring`] with an `EINTR`-tolerant `submit_and_wait` --
/// implemented as a trait on the foreign `ququmatz` type (rather than a
/// free function) since both [`Sender`] and [`Reader`] need it on their own
/// ring and neither owns the other's, so there's no single first-party
/// struct to hang it off of.
trait IoUringExt {
    fn submit_and_wait_retry(&mut self, min_complete: u32) -> Result<u32, ConnError>;
}

impl IoUringExt for IoUring {
    /// `submit_and_wait`, transparently retrying on `EINTR`.
    ///
    /// A signal landing on whichever thread happens to be parked in
    /// `io_uring_enter` -- most commonly `SIGCHLD` when the process
    /// elsewhere spawns a subprocess (e.g. the statusbar's module readers
    /// shelling out to `pactl`/`brightnessctl`/`iw`) -- unparks the syscall
    /// with `-EINTR` even though nothing is actually wrong with the ring.
    /// Treat it the same way `pty::Pty`'s write path already does: not an
    /// error, just retry.
    fn submit_and_wait_retry(&mut self, min_complete: u32) -> Result<u32, ConnError> {
        const EINTR: i32 = 4;
        loop {
            match self.submit_and_wait(min_complete) {
                Ok(n) => return Ok(n),
                // `submit_and_wait` calls `io_uring_enter` directly and `?`s
                // the raw `Errno` through `From<Errno> for ququmatz::Error`,
                // so an interrupted enter surfaces as the bare
                // `Error::Syscall(errno)` variant -- *not*
                // `Error::Submit(SubmitError::Syscall(..))` (that shape only
                // comes from `push`'s queue-full path). Matching only the
                // wrong shape meant this retry never actually fired.
                Err(
                    ququmatz::Error::Syscall(errno)
                    | ququmatz::Error::Submit(ququmatz::SubmitError::Syscall(errno)),
                ) if errno.raw() == EINTR => {}
                Err(e) => return Err(ConnError::Submit(e)),
            }
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use std::error::Error as _;

    fn io_err(msg: &str) -> io::Error {
        io::Error::other(msg.to_string())
    }

    #[test]
    fn display_messages_for_every_variant() {
        assert_eq!(
            ConnError::Env("WAYLAND_DISPLAY").to_string(),
            "environment variable WAYLAND_DISPLAY is not set"
        );
        assert_eq!(
            ConnError::NoCompletion.to_string(),
            "io_uring returned no completion after wait"
        );
        assert_eq!(
            ConnError::Closed.to_string(),
            "the compositor closed the connection"
        );
        assert_eq!(
            ConnError::Connect(io_err("refused")).to_string(),
            "connect to compositor failed: refused"
        );
        assert_eq!(
            ConnError::Io(io_err("broken pipe")).to_string(),
            "socket I/O failed: broken pipe"
        );
    }

    #[test]
    fn display_delegates_to_inner_ququmatz_error() {
        let inner = ququmatz::Error::Syscall(ququmatz::Errno::EAGAIN);
        assert_eq!(
            ConnError::Setup(inner).to_string(),
            format!("io_uring setup failed: {inner}")
        );
        assert_eq!(
            ConnError::Register(inner).to_string(),
            format!("provided-buffer registration failed: {inner}")
        );
        assert_eq!(
            ConnError::Submit(inner).to_string(),
            format!("io_uring submit/wait failed: {inner}")
        );
    }

    #[test]
    fn display_delegates_to_inner_sys_error() {
        let inner = SysError::Socket(io_err("nope"));
        let msg = inner.to_string();
        assert_eq!(ConnError::Sys(inner).to_string(), msg);
    }

    #[test]
    fn source_present_only_for_wrapped_errors() {
        assert!(ConnError::Env("X").source().is_none());
        assert!(ConnError::NoCompletion.source().is_none());
        assert!(ConnError::Closed.source().is_none());

        assert!(ConnError::Connect(io_err("x")).source().is_some());
        assert!(ConnError::Io(io_err("x")).source().is_some());

        let inner = ququmatz::Error::Syscall(ququmatz::Errno::EAGAIN);
        assert!(ConnError::Setup(inner).source().is_some());
        assert!(ConnError::Register(inner).source().is_some());
        assert!(ConnError::Submit(inner).source().is_some());

        assert!(
            ConnError::Sys(SysError::Memfd(io_err("x")))
                .source()
                .is_some()
        );
    }

    #[test]
    fn from_sys_error_wraps_as_sys_variant() {
        let e: ConnError = SysError::Mmap(io_err("x")).into();
        assert!(matches!(e, ConnError::Sys(SysError::Mmap(_))));
    }
}

#[cfg(test)]
mod recv_outcome_tests {
    use super::*;

    #[test]
    fn disconnect_errnos_are_classified_as_closure() {
        // The errnos a compositor crash/kill actually surfaces.
        for errno in [104, 107, 32, 103, 108] {
            assert!(
                Reader::errno_is_disconnect(errno),
                "errno {errno} should read as a disconnect"
            );
        }
    }

    #[test]
    fn genuine_io_errors_are_not_classified_as_closure() {
        // EAGAIN, EINTR, EBADF, ENOMEM, EFAULT: real faults that must still
        // propagate as errors rather than be swallowed as a clean hangup.
        for errno in [11, 4, 9, 12, 14] {
            assert!(
                !Reader::errno_is_disconnect(errno),
                "errno {errno} must not be mistaken for a disconnect"
            );
        }
    }

    #[test]
    fn closed_constant_reports_closure_without_progress() {
        // `CLOSED` is a const, so these are checkable at compile time --
        // a const block makes that explicit and keeps the guarantee even if
        // the test is never run.
        const { assert!(RecvOutcome::CLOSED.closed) };
        const { assert!(!RecvOutcome::CLOSED.absorbed) };
    }

    #[test]
    fn default_outcomes_are_inert() {
        let recv = RecvOutcome::default();
        assert!(!recv.closed && !recv.absorbed);
        let reaped = Reaped::default();
        assert!(!reaped.closed && !reaped.pushed && !reaped.absorbed);
    }

    /// The exact regression this work exists for: a batch that reaps
    /// completions but absorbs and pushes nothing is barren, and a run of
    /// them must trip the watchdog rather than spin forever.
    #[test]
    fn barren_reaps_wedge_the_guard() {
        let barren = Reaped::default();
        let mut guard = ProgressGuard::with_limit(8);
        let mut outcome = Progress::Continue;
        for _ in 0..8 {
            outcome = guard.record(barren.pushed || barren.absorbed);
        }
        assert_eq!(outcome, Progress::Wedged);
    }

    /// ...whereas a batch that absorbed bytes but hasn't yet produced a whole
    /// message (partial message split across recvs) counts as progress and
    /// must never trip it.
    #[test]
    fn partial_message_absorption_counts_as_progress() {
        let partial = Reaped {
            closed: false,
            pushed: false,
            absorbed: true,
        };
        let mut guard = ProgressGuard::with_limit(4);
        for _ in 0..1_000 {
            assert_eq!(
                guard.record(partial.pushed || partial.absorbed),
                Progress::Continue
            );
        }
    }
}

/// `io_uring` submission-queue depth (per ring).
const RING_ENTRIES: u32 = 16;

/// Completion-queue depth — larger than the SQ. A multishot `recvmsg` burst
/// (e.g. the registry advertising ~30 globals at once) can post CQEs faster than
/// we drain them; oversize the CQ so it can't overflow and drop completions.
const RING_CQ_ENTRIES: u32 = 64;

/// Provided-buffer pool for the recv ring: `POOL_BUFFERS` buffers of
/// `POOL_BUF_SIZE` bytes, group id `POOL_BGID`. Count must be a power of two.
const POOL_BGID: u16 = 0;
const POOL_BUFFERS: u32 = 16;
const POOL_BUF_SIZE: u32 = 8192;

/// No peer name on a connected socket.
const RECV_NAMELEN: u32 = 0;
/// Max fds we accept in one message's ancillary data.
const MAX_FDS: usize = 16;

/// Object ids of the two objects whose events carry strings, so the reader
/// forwards them as raw bytes for the main thread to re-parse:
///
///   * `wl_display` (always id 1) — the `error` event has a message string.
///   * `wl_registry` (id 2, the first id we allocate in `bind_globals`) — the
///     `global` event has an interface-name string.
///
/// All other events we care about (pointer, configure, ping, close, seat caps)
/// have only fixed `u32`/`i32` args and are inlined.
pub(crate) const DISPLAY_ID: u32 = 1;
pub(crate) const REGISTRY_ID: u32 = 2;

/// `user_data` tags on the rx ring, so the reader can tell its two kinds of
/// completion apart: the multishot recv on the socket vs. the one-shot poll on
/// the key-repeat timerfd. The recv keeps 0 (its default); the timer poll uses
/// a sentinel that no buffer-id or recv ever produces.
const UD_RECV: u64 = 0;
const UD_TIMER: u64 = 1;

/// Marker type whose sole job is to host [`WaylandConn::connect`].
///
/// The operation isn't a method on any value that exists yet (it *builds*
/// the values), so it lives as an associated fn on a zero-sized namespace
/// type rather than a bare top-level `connect()`.
///
/// Named `WaylandConn` (not the shorter `Conn`) to stay unambiguous next to
/// `dbus::conn::DbusConn`, the unrelated D-Bus connection type -- the two used
/// to share a bare name, which made grep/doc navigation and dependency
/// tooling (`cargo graph`) misattribute edges between them.
pub struct WaylandConn;

impl WaylandConn {
    /// Build the `AF_UNIX` socket, the two rings, connect, register the recv
    /// pool, arm the first multishot recv, and create the (disarmed)
    /// key-repeat timer. Returns the [`Sender`] and [`RepeatTimer`] (main
    /// thread) and the [`Reader`] (its own thread). The reader polls the
    /// timer's fd; the [`RepeatTimer`] that owns it stays on the main
    /// thread, which arms/disarms it as keys are held/released.
    pub fn connect() -> Result<(Sender, Reader, RepeatTimer), ConnError> {
        let runtime =
            std::env::var_os("XDG_RUNTIME_DIR").ok_or(ConnError::Env("XDG_RUNTIME_DIR"))?;
        let display = std::env::var_os("WAYLAND_DISPLAY")
            .unwrap_or_else(|| std::ffi::OsString::from("wayland-0"));
        let mut path = PathBuf::from(runtime);
        path.push(&display);

        // AF_UNIX stream socket (ququmatz's typed Socket is IPv4-only, so we make
        // the fd ourselves and adopt it).
        let sock_owned = Sender::make_unix_socket()?;
        let sock = RawFd::from_raw(sock_owned.as_raw_fd() as usize);

        // Two rings on the same fd: the send ring (main thread) and the recv ring
        // (reader thread). They're built differently — see the build_* functions.
        let mut ring_tx = Sender::build_tx_ring()?;
        let mut ring_rx = Reader::build_rx_ring()?;

        // Connect once (on the tx ring — any ring works, the socket is shared).
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let addr = sys::SockAddrUn::new(path_bytes);
        // SAFETY: `addr` outlives the blocking submit below.
        let cqe = Sender::run_one(&mut ring_tx, Sqe::connect(sock, addr.as_bytes()))?;
        if cqe < 0 {
            return Err(ConnError::Connect(io::Error::from_raw_os_error(-cqe)));
        }

        // Recv pool lives on the rx ring.
        let pool = ring_rx
            .register_provided_buffers(POOL_BGID, POOL_BUFFERS, POOL_BUF_SIZE)
            .map_err(ConnError::Register)?;

        // Persistent recv header: no name, control region wide enough for a
        // SCM_RIGHTS cmsg of up to MAX_FDS fds. Buffer-select supplies the data
        // buffer from the pool, so the iov stays empty.
        let recv_controllen = sys::IncomingControl::reserve_len(MAX_FDS) as u32;
        let mut recv_hdr = Box::new(MsgHdr::default());
        recv_hdr.msg_namelen = RECV_NAMELEN;
        recv_hdr.msg_controllen = recv_controllen as usize;

        // Key-repeat clock. The main thread arms/disarms it as keys go down/up;
        // the reader polls it. Unlike the socket (whose `OwnedFd` lives in the
        // `Sender`, i.e. as long as the connection), the `RepeatTimer` is handed
        // back to the caller — and keyboard-less callers drop it immediately. So
        // the reader gets its own dup rather than a borrowed raw integer,
        // otherwise that drop closes the fd it's parked on and the number gets
        // recycled under it. See `RepeatTimer::dup_fd`.
        let timer = RepeatTimer::new()?;
        let timer_fd = timer.dup_fd()?;

        // The reader arms its own multishot recv as the first thing it does on its
        // own thread — crucial, because `single_issuer`/`defer_taskrun` bind a ring
        // to the thread that first submits to it. The rx ring must only ever be
        // touched by the reader thread, the tx ring only by the main thread.
        let reader = Reader {
            sock,
            timer_fd,
            ring: ring_rx,
            pool,
            recv_hdr,
            recv_controllen,
            inbox: Vec::with_capacity(POOL_BUF_SIZE as usize),
            pending_fds: Vec::new(),
        };

        let sender = Sender {
            sock,
            sock_owned,
            ring: ring_tx,
            next_id: 2,
        };

        Ok((sender, reader, timer))
    }
}

// ---------------------------------------------------------------------------
// Sender — the send half, on the main thread.
// ---------------------------------------------------------------------------

/// Owns the send ring and the socket fd. Allocates object ids and submits
/// requests. Stays on the main thread.
///
/// # Safety
///
/// While the underlying `io_uring` ring is `!Send`, the `Sender` is always used
/// from the main/event-loop thread. The `Send` impl is needed so `Arc<Mutex<Sender>>`
/// can satisfy `Sync` bounds in winit's `Window` type.
pub struct Sender {
    sock: RawFd,
    /// Keeps the socket open for both rings; closed when the Sender drops.
    sock_owned: OwnedFd,
    ring: IoUring,
    /// Next client object id. Id 1 is `wl_display`, so we start at 2.
    next_id: u32,
}

// SAFETY: The Sender is always used from the thread that created it
// (the main/event-loop thread). It's wrapped in `Arc<Mutex<>>` only for
// structural reasons — io_uring ops are never submitted from other threads.
// `ring: IoUring` is the only non-Send field and is exactly the field this
// impl exists to paper over (clippy's non_send_fields_in_send_ty lint has
// no other way to express "deliberately unsafe-impl-Send, see the comment
// above" than to allow it at the impl site).
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for Sender {}

impl std::os::fd::AsRawFd for Sender {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.sock_owned.as_raw_fd()
    }
}

impl std::os::fd::AsFd for Sender {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        self.sock_owned.as_fd()
    }
}

impl Sender {
    /// Build the **send** ring. It's only ever touched by the main thread, so
    /// the `single_issuer` + `defer_taskrun` optimizations apply cleanly.
    ///
    /// Was a free `build_tx_ring()` function -- moved onto `Sender` since it
    /// builds the ring `Sender` owns.
    fn build_tx_ring() -> Result<IoUring, ConnError> {
        IoUring::builder(RING_ENTRIES)
            .single_issuer()
            .defer_taskrun()
            .clamp()
            .cq_entries(RING_CQ_ENTRIES)
            .build()
            .map_err(ConnError::Setup)
    }

    /// Submit one SQE, wait, return the raw CQE result. Used only during
    /// connect, before any multishot recv is armed.
    ///
    /// Was a free `run_one(ring, sqe)` function -- moved onto `Sender` since
    /// callers only ever pass the send ring here.
    fn run_one(ring: &mut IoUring, sqe: Sqe) -> Result<i32, ConnError> {
        ring.push(sqe).map_err(ConnError::Submit)?;
        ring.submit_and_wait_retry(1)?;
        let cqe = ring.complete().ok_or(ConnError::NoCompletion)?;
        Ok(cqe.result)
    }

    /// Create an `AF_UNIX`, `SOCK_STREAM` socket and wrap it in an `OwnedFd`.
    ///
    /// Was a free `make_unix_socket()` function -- moved onto `Sender` since
    /// it produces the socket `Sender` (and `Reader`) share ownership of.
    fn make_unix_socket() -> Result<OwnedFd, ConnError> {
        use std::os::fd::FromRawFd;
        const SOCK_STREAM: i32 = 1;
        // `SOCK_CLOEXEC` (02000000): the Wayland connection must NOT be inherited
        // by processes the PTY shell spawns. Without it, a shell child that
        // outlives the terminal keeps this socket open, so the compositor never
        // sees the connection close and leaves a frozen window behind after we
        // exit. Setting it atomically at socket() creation avoids the
        // fork-between-socket-and-fcntl race a later FD_CLOEXEC would have.
        const SOCK_CLOEXEC: i32 = 0o2_000_000;
        // SAFETY: standard socket(2) call; we adopt the returned fd as owned.
        let fd = unsafe { libc_socket(sys::AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(ConnError::Sys(SysError::Socket(io::Error::last_os_error())));
        }
        // SAFETY: socket() returned a fresh owned fd.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Reserve a fresh object id for an object we're about to create, typed as
    /// whichever [`ObjectId`] the call site asks for:
    /// `let s: SurfaceId = sender.alloc_id();`.
    pub fn alloc_id<T: ObjectId>(&mut self) -> T {
        let id = self.next_id;
        self.next_id += 1;
        T::from_raw(id)
    }

    /// Reserve a block of `count` consecutive object ids for a window's
    /// exclusive use. Returns `(start, end_exclusive)`. The window hands these
    /// out via [`WindowSender::alloc_id`] without touching the connection.
    pub const fn alloc_id_block(&mut self, count: u32) -> (u32, u32) {
        let start = self.next_id;
        self.next_id += count;
        (start, start + count)
    }

    /// Send a finished request with no fds.
    pub fn send(&mut self, msg: &[u8]) -> Result<(), ConnError> {
        self.send_with_fds(msg, &[])
    }

    /// Send a finished request, attaching `fds` as `SCM_RIGHTS` ancillary data.
    pub fn send_with_fds(&mut self, msg: &[u8], fds: &[BorrowedFd<'_>]) -> Result<(), ConnError> {
        let mut out = OutgoingMessage::new(msg, fds);
        let hdr = out.header();
        // SAFETY: `hdr` points into `out`/`msg`; the SENDMSG SQE holds a raw
        // pointer to `hdr`. We block until completion below, so all stay alive
        // -- `out`/`msg` are kept alive by simply staying in scope past the
        // blocking wait, so there's no separate keep-alive binding needed.
        let sqe = unsafe { Sqe::sendmsg_ptr(self.sock, &raw const hdr, MsgFlags::NOSIGNAL) };
        self.ring.push(sqe).map_err(ConnError::Submit)?;
        self.ring.submit_and_wait_retry(1)?;
        let cqe = self.ring.complete().ok_or(ConnError::NoCompletion)?;
        drop(out);
        if cqe.result < 0 {
            return Err(ConnError::Io(io::Error::from_raw_os_error(-cqe.result)));
        }
        Ok(())
    }

    /// Convenience: build → finish → send.
    pub fn request(&mut self, b: MessageBuilder) -> Result<(), ConnError> {
        self.send(&b.finish())
    }

    /// Duplicate the underlying socket fd. The caller owns the returned fd —
    /// the original stays open (it's the `sock_owned` field and the `io_uring`
    /// rings still reference it).
    pub fn socket_fd(&self) -> io::Result<OwnedFd> {
        self.sock_owned.try_clone()
    }

    /// Create a stub `Sender` that panics on any I/O attempt.
    /// Used by the winit-paint backend before the real connection is wired up.
    #[doc(hidden)]
    #[must_use]
    pub fn stub() -> Self {
        fn dummy_socket() -> OwnedFd {
            std::fs::File::open("/dev/null").unwrap().into()
        }
        Self {
            sock: RawFd::from_raw(dummy_socket().as_raw_fd() as usize),
            sock_owned: dummy_socket(),
            ring: IoUring::builder(2).clamp().cq_entries(4).build().unwrap(),
            next_id: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Reader — the receive half, on its own thread.
// ---------------------------------------------------------------------------

/// Owns the receive ring and buffer pool. Parses the socket byte stream into
/// whole Wayland messages and pushes each as a [`RawEvent`]. Runs on its own
/// thread via [`run`](Reader::run).
pub struct Reader {
    sock: RawFd,
    /// The key-repeat timerfd, polled on the rx ring.
    ///
    /// Owned here as a *dup* of the main thread's `RepeatTimer` fd, not a bare
    /// integer borrowed from it. The reader polls this fd for the whole life of
    /// the connection, but callers with no keyboard (statusbar, wallpaper,
    /// shot, clipboard) drop their `RepeatTimer` as soon as their constructor
    /// returns — which used to close the fd out from under this thread and let
    /// its number be recycled by the next `open`, turning the poll into a
    /// permanently-ready busy-loop (or an `EBADF` that killed the reader).
    /// Owning a dup makes that impossible; see [`RepeatTimer::dup_fd`].
    ///
    /// Both halves still share one open file description, so `arm`/`disarm` on
    /// the main thread drives this poll exactly as before.
    timer_fd: OwnedFd,
    ring: IoUring,
    pool: ProvidedBufferRing,
    recv_hdr: Box<MsgHdr>,
    recv_controllen: u32,
    inbox: Vec<u8>,
    pending_fds: Vec<OwnedFd>,
}

// SAFETY: the Reader owns its ring, pool, header, buffers, and timerfd outright;
// nothing is shared. The raw `sock` fd is an integer both halves submit against,
// which is sound — io_uring rings are independent of the fds they operate on,
// and recv (this thread) and send (the main thread) are separate socket
// directions. `timer_fd` is this thread's own dup of the key-repeat timer; the
// main thread arms/disarms the shared open file description while this thread
// only polls/drains it, never raced.
//
// clippy's `non_send_fields_in_send_ty` flags `ring: IoUring` and
// `recv_hdr: Box<MsgHdr>` as not independently provably `Send` -- true, but
// irrelevant here: this whole struct (not just individual fields) moves to
// the reader thread once at spawn and is never touched from any other
// thread again, exactly the invariant this manual impl asserts.
#[allow(clippy::non_send_fields_in_send_ty)]
unsafe impl Send for Reader {}

impl Reader {
    /// Build the **recv** ring. We register its buffer pool from the main
    /// thread in `connect()` but submit/wait from the reader thread, so we
    /// must NOT use `single_issuer` — that flag binds the ring to the first
    /// task to touch it (registration included) and the reader's later
    /// submit would fail `-EEXIST`. We keep the oversized CQ for
    /// multishot-burst headroom.
    ///
    /// Was a free `build_rx_ring()` function -- moved onto `Reader` since it
    /// builds the ring `Reader` owns.
    fn build_rx_ring() -> Result<IoUring, ConnError> {
        IoUring::builder(RING_ENTRIES)
            .clamp()
            .cq_entries(RING_CQ_ENTRIES)
            .build()
            .map_err(ConnError::Setup)
    }

    /// Run the receive loop until the connection closes or `producer`'s consumer
    /// is dropped. Each whole Wayland message becomes one pushed [`RawEvent`].
    ///
    /// Must run on the thread that will own the rx ring for its whole life — it
    /// arms the multishot recv here so this thread is the ring's sole issuer
    /// (required by `single_issuer`/`defer_taskrun`).
    pub fn run(
        mut self,
        producer: &Producer<RawEvent>,
        waker: Option<&waker::Waker>,
    ) -> Result<(), ConnError> {
        // Arm the first multishot recv and the key-repeat timer poll, both from
        // *this* thread so it's the rx ring's sole issuer.
        self.arm_recv()?;
        self.arm_timer_poll()?;

        // Backstop against the loop spinning against a peer that has gone
        // away without producing a completion we recognise as EOF. See
        // `progress_guard` for the full story -- in short, a recv on a dead
        // socket completes instantly and forever, and without this the
        // thread pins a core while the main thread sleeps for good.
        let mut guard = ProgressGuard::new();

        loop {
            // Submit the (re-)arms and block for the next completion(s).
            //
            // `EINTR` (errno 4) is not a real failure -- any signal delivered
            // to this thread while it's parked in `io_uring_enter` (e.g.
            // `SIGCHLD` from a subprocess a caller spawned elsewhere, such as
            // the statusbar's `pactl`/`brightnessctl` module readers) unparks
            // the syscall with `-EINTR` even though the ring itself is fine.
            // `pty::Pty::run`'s write path already treats `EINTR` as
            // "nothing happened, retry" for the same reason; do the same
            // here instead of tearing down the whole connection.
            self.ring.submit_and_wait_retry(1)?;

            let reaped = self.reap_completions(producer)?;
            let (closed, mut pushed) = (reaped.closed, reaped.pushed);
            // Absorbing bytes counts as progress even when it doesn't yet
            // yield a whole message to push (a partial message split across
            // recvs is real forward motion, not futility).
            let mut progressed = reaped.pushed || reaped.absorbed;

            // Drain whole messages out of the inbox and forward them, then
            // forward any fds that arrived alongside them. Either can signal
            // "the consumer dropped, stop the loop" via `ControlFlow::Break`.
            match self.drain_inbox_events(producer) {
                core::ops::ControlFlow::Break(()) => return Ok(()),
                core::ops::ControlFlow::Continue(p) => {
                    pushed |= p;
                    progressed |= p;
                }
            }
            match self.forward_pending_fds(producer) {
                core::ops::ControlFlow::Break(()) => return Ok(()),
                core::ops::ControlFlow::Continue(p) => {
                    pushed |= p;
                    progressed |= p;
                }
            }

            // Wake the UI thread, which polls a shared eventfd rather than
            // blocking on this ring (it also waits on the dirty-snapshot ring).
            if pushed && let Some(w) = waker {
                w.signal();
            }

            if closed {
                if let Some(w) = waker {
                    w.signal();
                }
                return Ok(());
            }

            // Nothing absorbed, nothing pushed, repeatedly: the connection is
            // wedged. Stop rather than burn a core forever, and wake the main
            // thread so it observes the reader's exit instead of sleeping
            // through it -- the whole failure mode we're guarding against is
            // "reader spins, main thread never learns".
            if guard.record(progressed) == Progress::Wedged {
                eprintln!(
                    "wayland reader: no forward progress in {} consecutive completions; \
                     treating the connection as closed",
                    guard.limit()
                );
                if let Some(w) = waker {
                    w.signal();
                }
                return Ok(());
            }
        }
    }

    /// Reap every completion currently queued and route it by the
    /// `user_data` we tagged each arm with (timerfd poll vs. socket recv).
    /// Split out of `run` -- this was the loop's inner `while let` reaping
    /// block, a self-contained "drain the ring" step that doesn't need to
    /// share `run`'s complexity budget with the inbox-draining and
    /// fd-forwarding steps that follow it.
    ///
    /// Returns a [`Reaped`] summarising the batch -- see its docs; all three
    /// flags feed back into `run`'s end-of-loop wake/exit/watchdog logic.
    fn reap_completions(&mut self, producer: &Producer<RawEvent>) -> Result<Reaped, ConnError> {
        let mut reaped = Reaped::default();
        while let Some(cqe) = self.ring.complete() {
            if cqe.user_data == UD_TIMER {
                self.absorb_timer_cqe(cqe.result, producer)?;
                reaped.pushed = true;
            } else {
                let recv = self.absorb_recv_cqe(cqe.result, cqe.flags)?;
                reaped.closed |= recv.closed;
                reaped.absorbed |= recv.absorbed;
            }
        }
        Ok(reaped)
    }

    /// Drain whole Wayland messages out of `self.inbox` and forward each as
    /// a [`RawEvent`]. Split out of `run` for the same reason as
    /// [`Self::reap_completions`].
    ///
    /// Returns `Continue(pushed)` normally, or `Break(())` the moment
    /// `push_block` reports the consumer has dropped -- `run` treats that
    /// as "stop the whole receive loop", matching the original inline
    /// `return Ok(())`.
    fn drain_inbox_events(
        &mut self,
        producer: &Producer<RawEvent>,
    ) -> core::ops::ControlFlow<(), bool> {
        let mut pushed = false;
        while let Some(size) = WireEvent::parse(&self.inbox).map(|(_, s)| s) {
            let event = RawEvent::from_wire_msg(&self.inbox[..size]);
            self.inbox.drain(..size);
            // push_block waits if the consumer is behind; if the consumer
            // has dropped, push returns the value back and we stop.
            if producer.push_block(event).is_err() {
                return core::ops::ControlFlow::Break(());
            }
            pushed = true;
        }
        core::ops::ControlFlow::Continue(pushed)
    }

    /// Forward any fds that arrived with this recv batch as one `Fd` event,
    /// pushed after the message events [`Self::drain_inbox_events`] already
    /// pushed. Wayland attaches fds out of band to a single message
    /// (clipboard send/receive, keymap); in practice an fd-bearing event
    /// arrives as its own recvmsg, so the batch correlates with the message
    /// just pushed. The main thread consumes them by order.
    ///
    /// Same `ControlFlow` contract as [`Self::drain_inbox_events`].
    fn forward_pending_fds(
        &mut self,
        producer: &Producer<RawEvent>,
    ) -> core::ops::ControlFlow<(), bool> {
        if self.pending_fds.is_empty() {
            return core::ops::ControlFlow::Continue(false);
        }
        let fds = core::mem::take(&mut self.pending_fds);
        if producer.push_block(RawEvent::Fd(fds)).is_err() {
            return core::ops::ControlFlow::Break(());
        }
        core::ops::ControlFlow::Continue(true)
    }

    /// Arm (or re-arm) the multishot recvmsg against the buffer pool.
    fn arm_recv(&mut self) -> Result<(), ConnError> {
        // SAFETY: `recv_hdr` is boxed and owned by `self`, pinned for the whole
        // multishot; the kernel reads it per delivered buffer.
        let sqe = unsafe {
            Sqe::recvmsg_multishot(self.sock, self.recv_hdr.as_mut(), MsgFlags::default())
                .buffer_select(POOL_BGID)
                .user_data(UD_RECV)
        };
        self.ring.push(sqe).map_err(ConnError::Submit)
    }

    /// Arm (or re-arm) a one-shot poll for readability on the key-repeat
    /// timerfd. `poll_add` fires once per readiness, so we re-arm after each
    /// expiry — the same arm/reap/re-arm shape as the recv, but one-shot.
    fn arm_timer_poll(&mut self) -> Result<(), ConnError> {
        let timer_fd = RawFd::from_raw(self.timer_fd.as_raw_fd() as usize);
        let sqe = Sqe::poll_add(timer_fd, PollMask::IN).user_data(UD_TIMER);
        self.ring.push(sqe).map_err(ConnError::Submit)
    }

    /// Handle a timerfd-poll completion: drain the expiration count (clearing
    /// the level-triggered readiness so the re-armed poll won't busy-loop), push
    /// one [`RawEvent::Repeat`] tick, and re-arm the poll. One readiness edge yields
    /// one tick regardless of how many expiries piled up.
    fn absorb_timer_cqe(
        &mut self,
        result: i32,
        producer: &Producer<RawEvent>,
    ) -> Result<(), ConnError> {
        // A poll error (e.g. the fd went away) is fatal for the timer; surface
        // it like any other ring I/O error.
        if result < 0 {
            return Err(ConnError::Io(io::Error::from_raw_os_error(-result)));
        }
        RepeatTimer::drain_fd(self.timer_fd.as_raw_fd());
        // Best-effort: if the consumer dropped, the main loop is ending anyway.
        let _ = producer.push_block(RawEvent::Repeat);
        self.arm_timer_poll()
    }

    /// Absorb one multishot recvmsg completion into the inbox.
    fn absorb_recv_cqe(&mut self, result: i32, flags: CqeFlags) -> Result<RecvOutcome, ConnError> {
        let more = flags.contains(CqeFlags::MORE);

        if result < 0 {
            let errno = -result;
            // A peer that vanishes rudely (compositor killed or crashed)
            // reports the hangup as an errno on the recv rather than a clean
            // zero-length read. That is a *closed connection*, not an I/O
            // fault: returning `Err` here would propagate a scary error out
            // of an ordinary shutdown, and -- worse, before the guard existed
            // -- any errno we failed to classify left the loop re-arming
            // against a dead socket forever.
            if Self::errno_is_disconnect(errno) {
                return Ok(RecvOutcome::CLOSED);
            }
            return Err(ConnError::Io(io::Error::from_raw_os_error(errno)));
        }

        // A zero-length recv means EOF, full stop. We deliberately no longer
        // require `!more`: if the kernel reports EOF while the multishot is
        // still notionally armed, the connection is just as dead, and the old
        // `result == 0 && !more` form let exactly that case fall through to
        // the unconditional re-arm below -- the spin that pinned four cores.
        if result == 0 {
            return Ok(RecvOutcome::CLOSED);
        }

        let mut absorbed = false;
        if flags.contains(CqeFlags::BUFFER)
            && let Some(buf_id) = flags.buffer_id()
        {
            if let Some(parts) =
                self.pool
                    .recvmsg_parts(buf_id, result as u32, RECV_NAMELEN, self.recv_controllen)
            {
                if !parts.payload.is_empty() {
                    self.inbox.extend_from_slice(parts.payload);
                    absorbed = true;
                }
                // Any fds the message carried (in practice just the keymap fd
                // from wl_keyboard.keymap — the only fd-bearing event we get).
                // We don't parse the XKB keymap, so we just take ownership; they
                // close when the Reader drops. Retained, not used, by design.
                let mut fds = sys::IncomingControl::fds(parts.control);
                if !fds.is_empty() {
                    absorbed = true;
                }
                self.pending_fds.append(&mut fds);
            }
            self.pool.recycle_and_commit(buf_id);
        }

        // Re-arm when the kernel ends the multishot.
        if !more {
            self.arm_recv()?;
        }
        Ok(RecvOutcome {
            closed: false,
            absorbed,
        })
    }

    /// Is this errno the socket telling us the peer is gone, rather than a
    /// genuine I/O fault?
    ///
    /// An associated function rather than a method: it inspects nothing but
    /// its argument, and lives here (rather than on `ConnError`) because
    /// "which errnos mean the compositor went away" is knowledge the receive
    /// loop owns.
    const fn errno_is_disconnect(errno: i32) -> bool {
        // ECONNRESET: peer reset the connection (compositor killed).
        // ENOTCONN:   socket is no longer connected.
        // EPIPE:      write end gone.
        // ECONNABORTED / ESHUTDOWN: connection torn down under us.
        const ECONNRESET: i32 = 104;
        const ENOTCONN: i32 = 107;
        const EPIPE: i32 = 32;
        const ECONNABORTED: i32 = 103;
        const ESHUTDOWN: i32 = 108;
        matches!(
            errno,
            ECONNRESET | ENOTCONN | EPIPE | ECONNABORTED | ESHUTDOWN
        )
    }
}

/// Outcome of absorbing a single recv completion.
///
/// Replaces a bare `bool` return: the receive loop now needs to distinguish
/// "the peer hung up" from "this completion actually carried something", and
/// a pair of named fields beats a tuple of anonymous bools at the call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RecvOutcome {
    /// The peer has hung up; the loop should tear down.
    closed: bool,
    /// This completion delivered payload bytes or fds — i.e. real forward
    /// progress, as judged by [`ProgressGuard`].
    absorbed: bool,
}

impl RecvOutcome {
    /// The peer hung up and (necessarily) delivered nothing.
    const CLOSED: Self = Self {
        closed: true,
        absorbed: false,
    };
}

/// Summary of one full drain of the completion queue.
///
/// Named struct rather than the previous `(bool, bool)` tuple because it grew
/// a third flag: with two same-typed bools a tuple was already easy to
/// transpose silently, and `absorbed` vs `pushed` is exactly the distinction
/// the watchdog depends on being right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Reaped {
    /// The peer hung up during this batch.
    closed: bool,
    /// Something was pushed onto the SPSC ring (so the UI thread needs a wake).
    pushed: bool,
    /// Payload bytes or fds landed in the inbox — forward progress even if no
    /// whole message emerged yet.
    absorbed: bool,
}

/// Extends `ququmatz`'s `CqeFlags` with the provided-buffer-id decode --
/// was a free `buffer_id(flags)` function taking `CqeFlags` as its only
/// arg, the shape an extension trait exists for (`CqeFlags` is external,
/// so no inherent impl is possible).
trait CqeFlagsExt {
    /// Decode the buffer id from the CQE's flags (upper 16 bits), or `None`
    /// if the `BUFFER` bit isn't set.
    fn buffer_id(self) -> Option<u16>;
}

impl CqeFlagsExt for CqeFlags {
    fn buffer_id(self) -> Option<u16> {
        if self.contains(Self::BUFFER) {
            Some((self.bits() >> 16) as u16)
        } else {
            None
        }
    }
}

unsafe extern "C" {
    #[link_name = "socket"]
    fn libc_socket(domain: i32, ty: i32, protocol: i32) -> i32;
}

// ---------------------------------------------------------------------------
// MPSC request ring: windows → event loop
// ---------------------------------------------------------------------------

/// A request from a window to the event loop, which holds the real [`Sender`].
///
/// Windows don't own the Wayland connection — they push requests onto an MPSC
/// ring, and the event loop (the single thread that owns `&mut Sender`) drains
/// and executes them. This keeps `Sender` `&mut` — no locks, no shared state.
pub enum WindowRequest {
    /// Send raw bytes (a finished Wayland message) with no ancillary data.
    Send(Vec<u8>),
    /// Send raw bytes with file descriptors attached (e.g. shm pool creation).
    SendWithFds { msg: Vec<u8>, fds: Vec<OwnedFd> },
}

/// The window's handle into the MPSC request ring. Cloneable — each clone is an
/// independent producer that can push requests concurrently.
///
/// Created by the event loop at window-setup time, pre-loaded with a block of
/// pre-allocated object ids so the window can allocate surface/buffer/etc ids
/// without a synchronous round-trip through the ring.
pub struct WindowSender {
    pub tx: mpsc::Producer<WindowRequest>,
    pub next_id: u32,
    /// Exclusive end of the pre-allocated id block.
    pub id_end: u32,
    /// Object ids the server has released (via `wl_display.delete_id`) and that
    /// we can hand out again. Without this, churny allocators (per-frame
    /// callbacks, per-resize buffers/pools) would walk `next_id` off the end of
    /// the block and panic. Recycled ids are reused before `next_id` advances.
    pub free_ids: Vec<u32>,
}

// SAFETY: Each clone of WindowSender owns its own mpsc::Producer with its
// own Cell<usize> cursor. MPSC push is lock-free and thread-safe: concurrent
// `&self` calls on different clones target distinct ring slots. The only
// shared state is the ring buffer itself, which is lock-free.
unsafe impl Sync for WindowSender {}

impl Clone for WindowSender {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            // Clones share the parent's id range — for multi-window, each
            // window gets its own `WindowSender` from the event loop with a
            // distinct pre-allocated block.
            next_id: self.next_id,
            id_end: self.id_end,
            free_ids: Vec::new(),
        }
    }
}

impl WindowSender {
    /// Reserve a fresh object id, reusing a server-released id when available
    /// and otherwise drawing from this window's pre-allocated block.
    pub fn alloc_id<T: ObjectId>(&mut self) -> T {
        if let Some(id) = self.free_ids.pop() {
            return T::from_raw(id);
        }
        assert!(
            self.next_id < self.id_end,
            "out of pre-allocated object ids"
        );
        let id = self.next_id;
        self.next_id += 1;
        T::from_raw(id)
    }

    /// Return an id to the free-list once the server confirms deletion via
    /// `wl_display.delete_id`. Only ids inside this window's block are recycled.
    pub fn recycle_id(&mut self, id: u32) {
        if id < self.id_end {
            self.free_ids.push(id);
        }
    }

    /// Push a finished Wayland message (no fds) onto the request ring.
    ///
    /// Non-blocking `push`, not `push_block`: producer and consumer both run
    /// on the event-loop thread (this is called from `handle_event`, drained
    /// by `drain_requests` on the same thread just before the next `wait`),
    /// so blocking here would deadlock waiting for a drain that can't happen
    /// until we return.
    ///
    /// A drop can mean two different things and we can't tell them apart
    /// from here: the consumer dropped (event loop exited, window closing —
    /// harmless) or the 64-slot ring is transiently full (would silently
    /// desync the Wayland protocol state — a real bug). Since we can't
    /// distinguish them, report the possibility rather than swallow it
    /// outright; a well-behaved event loop drains every tick so this should
    /// never actually fire outside shutdown.
    pub fn send(&self, msg: Vec<u8>) {
        if self.tx.push(WindowRequest::Send(msg)).is_err() {
            eprintln!(
                "platform: window request ring full or consumer gone; dropped a Wayland message"
            );
        }
    }

    /// Push a finished Wayland message with ancillary file descriptors.
    /// See [`WindowSender::send`] for why this drops (not blocks) on a full
    /// ring, and why we still report it.
    pub fn send_with_fds(&self, msg: Vec<u8>, fds: Vec<OwnedFd>) {
        if self
            .tx
            .push(WindowRequest::SendWithFds { msg, fds })
            .is_err()
        {
            eprintln!(
                "platform: window request ring full or consumer gone; dropped a Wayland message with fds"
            );
        }
    }

    /// Convenience: build → finish → send.
    pub fn request(&self, b: MessageBuilder) {
        self.send(b.finish());
    }
}

/// Drain all pending [`WindowRequest`]s from the MPSC ring and execute them on
/// `sender`. Called by the event loop each tick, *before* waiting for events, so
/// queued requests hit the wire before the compositor's next response.
impl Sender {
    /// Drain all pending [`WindowRequest`]s from `rx` and execute them on
    /// `self`. Called by the event loop each tick, before waiting for
    /// events, so queued requests hit the wire before the compositor's next
    /// response.
    pub fn drain_requests(
        &mut self,
        rx: &mut mpsc::Consumer<WindowRequest>,
    ) -> Result<(), ConnError> {
        while let Some(req) = rx.pop() {
            match req {
                WindowRequest::Send(msg) => self.send(&msg)?,
                WindowRequest::SendWithFds { msg, fds } => {
                    let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|fd| fd.as_fd()).collect();
                    self.send_with_fds(&msg, &borrowed)?;
                }
            }
        }
        Ok(())
    }
}
