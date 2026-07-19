#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
//! The Wayland connection, split across two threads by direction.
//!
//! All I/O to the compositor goes through io_uring (via `ququmatz`), over a
//! single `AF_UNIX` socket. To overlap input handling with rendering we use
//! **two independent io_uring rings on the same socket fd**:
//!
//! * [`Reader`] owns the receive ring and the provided-buffer pool. It runs on
//!   its own thread: arm a multishot `recvmsg`, block, reap completions, split
//!   the bytes into whole Wayland messages, and push each as a [`RawEvent`] onto a
//!   lock-free SPSC ring. Arm / reap / re-arm are all local to this ring, so the
//!   re-arm never crosses a thread boundary — no shared state.
//! * [`Sender`] owns the send ring and stays on the main thread. It allocates
//!   object ids and submits requests (`sendmsg`, optionally with SCM_RIGHTS
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
use crate::sys::{self, OutgoingMessage, RepeatTimer, SysError};
use crate::wire::{MessageBuilder, WireEvent};

/// A failure setting up or running the Wayland connection.
#[derive(Debug)]
pub enum ConnError {
    /// A required environment variable was missing.
    Env(&'static str),
    /// io_uring ring setup (`io_uring_setup` + mmaps) failed.
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
            ConnError::Env(var) => write!(f, "environment variable {var} is not set"),
            ConnError::Setup(e) => write!(f, "io_uring setup failed: {e}"),
            ConnError::Register(e) => write!(f, "provided-buffer registration failed: {e}"),
            ConnError::Submit(e) => write!(f, "io_uring submit/wait failed: {e}"),
            ConnError::Connect(e) => write!(f, "connect to compositor failed: {e}"),
            ConnError::NoCompletion => write!(f, "io_uring returned no completion after wait"),
            ConnError::Io(e) => write!(f, "socket I/O failed: {e}"),
            ConnError::Closed => write!(f, "the compositor closed the connection"),
            ConnError::Sys(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ConnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConnError::Setup(e) | ConnError::Register(e) | ConnError::Submit(e) => Some(e),
            ConnError::Connect(e) | ConnError::Io(e) => Some(e),
            ConnError::Sys(e) => Some(e),
            ConnError::Env(_) | ConnError::NoCompletion | ConnError::Closed => None,
        }
    }
}

impl From<SysError> for ConnError {
    fn from(e: SysError) -> Self {
        ConnError::Sys(e)
    }
}

/// `submit_and_wait`, transparently retrying on `EINTR`.
///
/// A signal landing on whichever thread happens to be parked in
/// `io_uring_enter` -- most commonly `SIGCHLD` when the process elsewhere
/// spawns a subprocess (e.g. the statusbar's module readers shelling out to
/// `pactl`/`brightnessctl`/`iw`) -- unparks the syscall with `-EINTR` even
/// though nothing is actually wrong with the ring. Treat it the same way
/// `pty::Pty`'s write path already does: not an error, just retry.
fn submit_and_wait_retry(ring: &mut IoUring, min_complete: u32) -> Result<u32, ConnError> {
    const EINTR: i32 = 4;
    loop {
        match ring.submit_and_wait(min_complete) {
            Ok(n) => return Ok(n),
            Err(ququmatz::Error::Submit(ququmatz::SubmitError::Syscall(errno)))
                if errno.raw() == EINTR => {}
            Err(e) => return Err(ConnError::Submit(e)),
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

/// io_uring submission-queue depth (per ring).
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

/// Marker type whose sole job is to host [`Conn::connect`] -- the operation
/// isn't a method on any value that exists yet (it *builds* the values), so
/// it lives as an associated fn on a zero-sized namespace type rather than a
/// bare top-level `connect()`.
pub struct Conn;

impl Conn {
    /// Build the AF_UNIX socket, the two rings, connect, register the recv
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

        // Key-repeat clock. Owned by the main thread (which arms/disarms it as keys
        // go down/up); the reader only polls its fd, so it gets the raw integer —
        // exactly how the socket fd is shared across the two threads.
        let timer = RepeatTimer::new()?;
        let timer_fd = RawFd::from_raw(timer.fd().as_raw_fd() as usize);

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
            _sock_owned: sock_owned,
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
/// While the underlying io_uring ring is `!Send`, the `Sender` is always used
/// from the main/event-loop thread. The `Send` impl is needed so `Arc<Mutex<Sender>>`
/// can satisfy `Sync` bounds in winit's `Window` type.
pub struct Sender {
    sock: RawFd,
    /// Keeps the socket open for both rings; closed when the Sender drops.
    _sock_owned: OwnedFd,
    ring: IoUring,
    /// Next client object id. Id 1 is wl_display, so we start at 2.
    next_id: u32,
}

// SAFETY: The Sender is always used from the thread that created it
// (the main/event-loop thread). It's wrapped in `Arc<Mutex<>>` only for
// structural reasons — io_uring ops are never submitted from other threads.
unsafe impl Send for Sender {}

impl std::os::fd::AsRawFd for Sender {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self._sock_owned.as_raw_fd()
    }
}

impl std::os::fd::AsFd for Sender {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        self._sock_owned.as_fd()
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
        submit_and_wait_retry(ring, 1)?;
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
    pub fn alloc_id_block(&mut self, count: u32) -> (u32, u32) {
        let start = self.next_id;
        self.next_id += count;
        (start, start + count)
    }

    /// Send a finished request with no fds.
    pub fn send(&mut self, msg: Vec<u8>) -> Result<(), ConnError> {
        self.send_with_fds(msg, &[])
    }

    /// Send a finished request, attaching `fds` as SCM_RIGHTS ancillary data.
    pub fn send_with_fds(&mut self, msg: Vec<u8>, fds: &[BorrowedFd<'_>]) -> Result<(), ConnError> {
        let mut out = OutgoingMessage::new(&msg, fds);
        let hdr = out.header();
        // SAFETY: `hdr` points into `out`/`msg`; the SENDMSG SQE holds a raw
        // pointer to `hdr`. We block until completion below, so all stay alive.
        let sqe = unsafe { Sqe::sendmsg_ptr(self.sock, &hdr, MsgFlags::NOSIGNAL) };
        self.ring.push(sqe).map_err(ConnError::Submit)?;
        submit_and_wait_retry(&mut self.ring, 1)?;
        let cqe = self.ring.complete().ok_or(ConnError::NoCompletion)?;
        let _keep_alive = (&hdr, &out, &msg);
        if cqe.result < 0 {
            return Err(ConnError::Io(io::Error::from_raw_os_error(-cqe.result)));
        }
        Ok(())
    }

    /// Convenience: build → finish → send.
    pub fn request(&mut self, b: MessageBuilder) -> Result<(), ConnError> {
        self.send(b.finish())
    }

    /// Duplicate the underlying socket fd. The caller owns the returned fd —
    /// the original stays open (it's the `_sock_owned` field and the io_uring
    /// rings still reference it).
    pub fn socket_fd(&self) -> io::Result<OwnedFd> {
        self._sock_owned.try_clone()
    }

    /// Create a stub `Sender` that panics on any I/O attempt.
    /// Used by the winit-paint backend before the real connection is wired up.
    #[doc(hidden)]
    pub fn stub() -> Self {
        fn dummy_socket() -> OwnedFd {
            std::fs::File::open("/dev/null").unwrap().into()
        }
        Self {
            sock: RawFd::from_raw(dummy_socket().as_raw_fd() as usize),
            _sock_owned: dummy_socket(),
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
    /// The key-repeat timerfd, polled on the rx ring. Owned (as a `RepeatTimer`)
    /// by the main thread; here it's a raw integer we only poll and drain — the
    /// same shared-fd arrangement as `sock`.
    timer_fd: RawFd,
    ring: IoUring,
    pool: ProvidedBufferRing,
    recv_hdr: Box<MsgHdr>,
    recv_controllen: u32,
    inbox: Vec<u8>,
    pending_fds: Vec<OwnedFd>,
}

// SAFETY: the Reader owns its ring, pool, header, and buffers outright; nothing
// is shared. The raw `sock`/`timer_fd` fds are integers both halves submit
// against, which is sound — io_uring rings are independent of the fds they
// operate on, recv (this thread) and send (the main thread) are separate socket
// directions, and the timerfd is only ever armed (main) vs. polled/drained (this
// thread), never raced.
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
        producer: Producer<RawEvent>,
        waker: Option<waker::Waker>,
    ) -> Result<(), ConnError> {
        // Arm the first multishot recv and the key-repeat timer poll, both from
        // *this* thread so it's the rx ring's sole issuer.
        self.arm_recv()?;
        self.arm_timer_poll()?;

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
            submit_and_wait_retry(&mut self.ring, 1)?;

            let mut closed = false;
            let mut pushed = false;
            while let Some(cqe) = self.ring.complete() {
                // Route by the user_data we tagged each arm with: the timerfd
                // poll vs. the socket recv. The timer poll has no buffer/MORE
                // flags, so we must split on user_data, not on the flags.
                if cqe.user_data == UD_TIMER {
                    self.absorb_timer_cqe(cqe.result, &producer)?;
                    pushed = true;
                } else if self.absorb_recv_cqe(cqe.result, cqe.flags)? {
                    closed = true;
                }
            }

            // Drain whole messages out of the inbox and forward them.
            while let Some(size) = WireEvent::parse(&self.inbox).map(|(_, s)| s) {
                let event = RawEvent::from_wire_msg(&self.inbox[..size]);
                self.inbox.drain(..size);
                // push_block waits if the consumer is behind; if the consumer
                // has dropped, push returns the value back and we stop.
                if producer.push_block(event).is_err() {
                    return Ok(());
                }
                pushed = true;
            }

            // Forward any fds that arrived with this recv batch as one `Fd`
            // event, pushed after the message events above. Wayland attaches
            // fds out of band to a single message (clipboard send/receive,
            // keymap); in practice an fd-bearing event arrives as its own
            // recvmsg, so the batch correlates with the message just pushed.
            // The main thread consumes them by order.
            if !self.pending_fds.is_empty() {
                let fds = core::mem::take(&mut self.pending_fds);
                if producer.push_block(RawEvent::Fd(fds)).is_err() {
                    return Ok(());
                }
                pushed = true;
            }

            // Wake the UI thread, which polls a shared eventfd rather than
            // blocking on this ring (it also waits on the dirty-snapshot ring).
            if pushed {
                if let Some(w) = &waker {
                    w.signal();
                }
            }

            if closed {
                if let Some(w) = &waker {
                    w.signal();
                }
                return Ok(());
            }
        }
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
        let sqe = Sqe::poll_add(self.timer_fd, PollMask::IN).user_data(UD_TIMER);
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
        RepeatTimer::drain_fd(self.timer_fd.as_i32());
        // Best-effort: if the consumer dropped, the main loop is ending anyway.
        let _ = producer.push_block(RawEvent::Repeat);
        self.arm_timer_poll()
    }

    /// Absorb one multishot recvmsg completion into the inbox. Returns `true` if
    /// the peer closed the connection.
    fn absorb_recv_cqe(&mut self, result: i32, flags: CqeFlags) -> Result<bool, ConnError> {
        let more = flags.contains(CqeFlags::MORE);
        if result < 0 {
            return Err(ConnError::Io(io::Error::from_raw_os_error(-result)));
        }
        if result == 0 && !more {
            return Ok(true); // peer closed, multishot over
        }

        if flags.contains(CqeFlags::BUFFER)
            && let Some(buf_id) = flags.buffer_id()
        {
            if let Some(parts) =
                self.pool
                    .recvmsg_parts(buf_id, result as u32, RECV_NAMELEN, self.recv_controllen)
            {
                self.inbox.extend_from_slice(parts.payload);
                // Any fds the message carried (in practice just the keymap fd
                // from wl_keyboard.keymap — the only fd-bearing event we get).
                // We don't parse the XKB keymap, so we just take ownership; they
                // close when the Reader drops. Retained, not used, by design.
                let mut fds = sys::IncomingControl::fds(parts.control);
                self.pending_fds.append(&mut fds);
            }
            self.pool.recycle_and_commit(buf_id);
        }

        // Re-arm when the kernel ends the multishot.
        if !more {
            self.arm_recv()?;
        }
        Ok(false)
    }
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
        if self.contains(CqeFlags::BUFFER) {
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
                WindowRequest::Send(msg) => self.send(msg)?,
                WindowRequest::SendWithFds { msg, fds } => {
                    let borrowed: Vec<BorrowedFd<'_>> = fds.iter().map(|fd| fd.as_fd()).collect();
                    self.send_with_fds(msg, &borrowed)?;
                }
            }
        }
        Ok(())
    }
}
