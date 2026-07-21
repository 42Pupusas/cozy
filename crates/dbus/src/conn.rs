//! A minimal, synchronous D-Bus connection over `io_uring`.
//!
//! This isn't a general-purpose D-Bus client — it's sized for what the
//! statusbar needs: connect to the system bus, `Hello`, and make a handful
//! of blocking method calls per poll (get a property, list objects). There's
//! no event loop, no signal subscriptions, no multithreading: one
//! `IoUring` ring, used exactly the way `platform::conn::Sender::run_one`
//! uses its send ring for the Wayland `connect` handshake — push one SQE,
//! `submit_and_wait(1)`, read the CQE.
//!
//! Every socket op (`connect`, `sendmsg`/`send`, `recv`) goes through
//! `io_uring` via `ququmatz`, matching this workspace's policy of routing all
//! I/O through `io_uring` rather than blocking `read`/`write` syscalls — see
//! `platform::conn` for the fuller rationale (this module's shape is a
//! stripped-down, single-ring version of that split-reader/sender design,
//! collapsed into one thread since a statusbar poll has no concurrent
//! traffic to overlap).

use std::fmt;
use std::io;
use std::os::fd::OwnedFd;

use ququmatz::types::MsgFlags;
use ququmatz::{IoUring, Sqe};

use crate::message::{self, BodyArg, MessageType, Prelude, Reply, PRELUDE_LEN};
use crate::sys;

/// Ring depth — one call in flight at a time, so this only ever needs a
/// couple of entries; oversized slightly the same way `platform::conn` does
/// for headroom, not because we expect real queuing.
const RING_ENTRIES: u32 = 8;
const RING_CQ_ENTRIES: u32 = 16;

/// A failure connecting to or calling out over the bus.
#[derive(Debug)]
pub enum DbusError {
    /// Neither `DBUS_SESSION_BUS_ADDRESS` nor the well-known system bus
    /// socket was usable (only relevant to [`Conn::session`]; `Conn::system`
    /// has one fixed path and reports [`DbusError::Connect`] instead).
    NoAddress,
    Setup(ququmatz::Error),
    Connect(io::Error),
    Io(io::Error),
    /// The peer closed the connection (0-byte recv when we expected data).
    Closed,
    /// The SASL handshake didn't say `OK`.
    AuthRejected(String),
    /// A reply didn't parse as a well-formed D-Bus message.
    Malformed,
    /// The call returned `MessageType::Error`.
    Reply {
        name: String,
    },
}

impl fmt::Display for DbusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAddress => write!(f, "could not determine D-Bus session bus address"),
            Self::Setup(e) => write!(f, "io_uring setup failed: {e}"),
            Self::Connect(e) => write!(f, "connect to bus failed: {e}"),
            Self::Io(e) => write!(f, "bus socket I/O failed: {e}"),
            Self::Closed => write!(f, "the bus closed the connection"),
            Self::AuthRejected(line) => write!(f, "SASL auth rejected: {line}"),
            Self::Malformed => write!(f, "malformed D-Bus message"),
            Self::Reply { name } => write!(f, "D-Bus error reply: {name}"),
        }
    }
}

impl std::error::Error for DbusError {}

/// A blocking connection to one bus (system or session), good for a series
/// of synchronous method calls from a single thread.
pub struct Conn {
    _sock: OwnedFd,
    sock_fd: ququmatz::types::RawFd,
    ring: IoUring,
    next_serial: u32,
}

impl Conn {
    /// Connect to the well-known system bus socket
    /// (`/var/run/dbus/system_bus_socket` — the standard path on every
    /// mainstream distro; there's no environment variable for it, unlike the
    /// session bus).
    pub fn system() -> Result<Self, DbusError> {
        Self::connect_path(b"/var/run/dbus/system_bus_socket")
    }

    /// Connect to the session bus, resolved from `DBUS_SESSION_BUS_ADDRESS`
    /// (supports the common `unix:path=...` and `unix:abstract=...` forms;
    /// anything else is rejected as [`DbusError::NoAddress`] rather than
    /// guessed at).
    pub fn session() -> Result<Self, DbusError> {
        let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").map_err(|_| DbusError::NoAddress)?;
        for part in addr.split(';') {
            let Some(rest) = part.strip_prefix("unix:") else {
                continue;
            };
            for kv in rest.split(',') {
                if let Some(path) = kv.strip_prefix("path=") {
                    return Self::connect_path(path.as_bytes());
                }
                if let Some(name) = kv.strip_prefix("abstract=") {
                    return Self::connect_abstract(name.as_bytes());
                }
            }
        }
        Err(DbusError::NoAddress)
    }

    fn connect_path(path: &[u8]) -> Result<Self, DbusError> {
        Self::connect_addr(&sys::SockAddrUn::new(path))
    }

    fn connect_abstract(name: &[u8]) -> Result<Self, DbusError> {
        Self::connect_addr(&sys::SockAddrUn::new_abstract(name))
    }

    fn connect_addr(addr: &sys::SockAddrUn) -> Result<Self, DbusError> {
        let sock = sys::make_unix_socket().map_err(DbusError::Connect)?;
        let sock_fd = sys::as_uring_fd(&sock);

        let mut ring = IoUring::builder(RING_ENTRIES)
            .clamp()
            .cq_entries(RING_CQ_ENTRIES)
            .build()
            .map_err(DbusError::Setup)?;

        let cqe = Self::run_one(&mut ring, Sqe::connect(sock_fd, addr.as_bytes()))?;
        if cqe < 0 {
            return Err(DbusError::Connect(io::Error::from_raw_os_error(-cqe)));
        }

        let mut conn = Self {
            _sock: sock,
            sock_fd,
            ring,
            next_serial: 1,
        };
        conn.authenticate()?;
        conn.hello()?;
        Ok(conn)
    }

    /// Submit one SQE, block for its completion, return the raw CQE result.
    fn run_one(ring: &mut IoUring, sqe: Sqe) -> Result<i32, DbusError> {
        ring.push(sqe).map_err(DbusError::Setup)?;
        Self::submit_and_wait_retry(ring, 1)?;
        let cqe = ring
            .complete()
            .ok_or_else(|| DbusError::Io(io::Error::other("no completion")))?;
        Ok(cqe.result)
    }

    /// `submit_and_wait`, retrying transparently on `EINTR` — same rationale
    /// as `platform::conn`'s copy: a signal (e.g. `SIGCHLD` from a sibling
    /// module shelling out to `pactl`) landing on this thread while parked in
    /// `io_uring_enter` unparks it with `-EINTR` even though nothing's wrong.
    fn submit_and_wait_retry(ring: &mut IoUring, min_complete: u32) -> Result<u32, DbusError> {
        const EINTR: i32 = 4;
        loop {
            match ring.submit_and_wait(min_complete) {
                Ok(n) => return Ok(n),
                Err(
                    ququmatz::Error::Syscall(errno)
                    | ququmatz::Error::Submit(ququmatz::SubmitError::Syscall(errno)),
                ) if errno.raw() == EINTR => {}
                Err(e) => return Err(DbusError::Setup(e)),
            }
        }
    }

    fn send_raw(&mut self, bytes: &[u8]) -> Result<(), DbusError> {
        let mut sent = 0;
        while sent < bytes.len() {
            // SAFETY: `bytes[sent..]` is valid for the duration of this
            // blocking submit+wait+complete, all within this call.
            let remaining = u32::try_from(bytes.len() - sent).unwrap_or(u32::MAX);
            let sqe = unsafe {
                Sqe::send_ptr(
                    self.sock_fd,
                    bytes[sent..].as_ptr(),
                    remaining,
                    MsgFlags::NOSIGNAL,
                )
            };
            let n = Self::run_one(&mut self.ring, sqe)?;
            if n < 0 {
                return Err(DbusError::Io(io::Error::from_raw_os_error(-n)));
            }
            if n == 0 {
                return Err(DbusError::Closed);
            }
            // `n` is a byte count `run_one` just confirmed is >= 0.
            sent += usize::try_from(n).unwrap_or(0);
        }
        Ok(())
    }

    /// Read exactly `buf.len()` bytes, growing `into` -- used both for the
    /// line-based SASL handshake and for full message frames.
    fn recv_exact(&mut self, n: usize) -> Result<Vec<u8>, DbusError> {
        let mut out = vec![0u8; n];
        let mut got = 0;
        while got < n {
            // SAFETY: `out[got..]` is valid for the duration of this blocking
            // submit+wait+complete.
            let remaining = u32::try_from(n - got).unwrap_or(u32::MAX);
            let sqe = unsafe {
                Sqe::recv_ptr(
                    self.sock_fd,
                    out[got..].as_mut_ptr(),
                    remaining,
                    MsgFlags::default(),
                )
            };
            let r = Self::run_one(&mut self.ring, sqe)?;
            if r < 0 {
                return Err(DbusError::Io(io::Error::from_raw_os_error(-r)));
            }
            if r == 0 {
                return Err(DbusError::Closed);
            }
            // `r` is a byte count `run_one` just confirmed is >= 0.
            got += usize::try_from(r).unwrap_or(0);
        }
        Ok(out)
    }

    /// Read one line (up to and including `\r\n`), a byte at a time — only
    /// used for the short SASL handshake (a handful of lines, ever), where
    /// simplicity matters far more than the syscall-per-byte cost.
    fn recv_line(&mut self) -> Result<String, DbusError> {
        let mut line = Vec::new();
        loop {
            let byte = self.recv_exact(1)?;
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        line.truncate(line.len() - 2);
        String::from_utf8(line).map_err(|_| DbusError::Malformed)
    }

    /// SASL `EXTERNAL` auth: prove identity via the hex-encoded uid (the
    /// mechanism the kernel actually authenticates via `SO_PEERCRED`; the
    /// hex uid on the wire is the identity being asserted, matched against
    /// the credential the kernel attached to the socket).
    fn authenticate(&mut self) -> Result<(), DbusError> {
        self.send_raw(&[0])?; // initial NUL byte, required before AUTH
        let uid = sys::effective_uid();
        let hex_uid = uid.to_string().bytes().fold(String::new(), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        self.send_raw(format!("AUTH EXTERNAL {hex_uid}\r\n").as_bytes())?;
        let line = self.recv_line()?;
        if !line.starts_with("OK ") {
            return Err(DbusError::AuthRejected(line));
        }
        self.send_raw(b"NEGOTIATE_UNIX_FD\r\n")?;
        let _ = self.recv_line()?; // "AGREE_UNIX_FD" or "ERROR" -- either way, proceed unix-fd-less
        self.send_raw(b"BEGIN\r\n")?;
        Ok(())
    }

    /// `org.freedesktop.DBus.Hello` — required as the first real message on
    /// any connection; the bus won't route anything else until this
    /// completes (it's how the connection is assigned its unique name).
    fn hello(&mut self) -> Result<(), DbusError> {
        self.call_raw(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            &[],
        )?;
        Ok(())
    }

    /// Make a method call and block for the reply. Returns
    /// [`DbusError::Reply`] if the peer replied with an `Error` message
    /// (rather than surfacing it as a successful-but-error-typed [`Reply`] —
    /// callers overwhelmingly want `?` to just propagate a bus error the
    /// same as a transport error).
    pub fn call(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: &[BodyArg],
    ) -> Result<Reply, DbusError> {
        let reply = self.call_raw(destination, path, interface, member, args)?;
        if reply.kind == MessageType::Error {
            let name = reply.error_name.unwrap_or_default();
            return Err(DbusError::Reply { name });
        }
        Ok(reply)
    }

    fn call_raw(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        args: &[BodyArg],
    ) -> Result<Reply, DbusError> {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        let msg = message::build_method_call(serial, destination, path, interface, member, args);
        self.send_raw(&msg)?;

        // The bus can (and does) interleave unsolicited signals with the
        // reply we're waiting for -- e.g. `NameAcquired` lands right after
        // `Hello`, and any service can broadcast `PropertiesChanged` at any
        // time. Reading exactly one message and assuming it's the reply
        // silently desyncs every call after the first stray signal (every
        // later "reply" is actually the previous call's real reply, or a
        // different signal, shifted by one). Loop, discarding anything
        // whose `REPLY_SERIAL` doesn't match this call's serial.
        loop {
            let reply = self.recv_one_message()?;
            if reply.reply_serial == Some(serial) {
                return Ok(reply);
            }
            // Not ours -- a signal, or (in principle) a stray reply to a
            // call we're no longer waiting on. Drop it and keep reading.
        }
    }

    /// Read one complete message frame off the wire (prelude, then exactly
    /// as many more bytes as the prelude says), regardless of what kind of
    /// message it turns out to be.
    fn recv_one_message(&mut self) -> Result<Reply, DbusError> {
        let prelude_bytes = self.recv_exact(PRELUDE_LEN)?;
        let prelude = Prelude::parse(&prelude_bytes).ok_or(DbusError::Malformed)?;
        let remaining = prelude.total_len() - PRELUDE_LEN;
        let rest = self.recv_exact(remaining)?;

        let mut full = prelude_bytes;
        full.extend_from_slice(&rest);
        message::parse_message(&full).ok_or(DbusError::Malformed)
    }

    /// Convenience: `org.freedesktop.DBus.Properties.Get`, unwrapping the
    /// single returned `Variant` — the overwhelmingly common shape every
    /// property read in this crate's callers wants.
    pub fn get_property(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        property: &str,
    ) -> Result<crate::value::Value, DbusError> {
        let reply = self.call(
            destination,
            path,
            "org.freedesktop.DBus.Properties",
            "Get",
            &[
                BodyArg::Str(interface.to_string()),
                BodyArg::Str(property.to_string()),
            ],
        )?;
        let values = reply.values();
        Ok(values.into_iter().next().map_or(
            crate::value::Value::Struct(Vec::new()),
            crate::value::Value::into_inner,
        ))
    }
}
