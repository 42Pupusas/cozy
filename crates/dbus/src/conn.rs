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

use crate::message::{BodyArg, MessageType, PRELUDE_LEN, Prelude, Reply};
use crate::session_address::SessionAddress;
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
    /// socket was usable (only relevant to [`DbusConn::session`]; `DbusConn::system`
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
///
/// Named `DbusConn` (not the shorter `Conn`) to stay unambiguous next to
/// `platform::conn::WaylandConn`, the unrelated Wayland connection type --
/// the two used to share a bare name, which made grep/doc navigation and
/// dependency tooling (`cargo graph`) misattribute edges between them.
pub struct DbusConn {
    _sock: OwnedFd,
    sock_fd: ququmatz::types::RawFd,
    ring: IoUring,
    next_serial: u32,
    /// Messages read off the wire while waiting for a specific call's reply
    /// that turned out to belong to someone else -- an unsolicited signal,
    /// or (for the tray watcher) an incoming method call from a client.
    ///
    /// `call_raw` used to just discard these, which was silently correct for
    /// a bar that only ever made outgoing calls. Once the tray watcher
    /// started reading back into the *same* connection it also serves
    /// incoming registrations on (`get_property` called from inside the
    /// receive loop's `register`/`refresh_changed_item` handlers), a
    /// `NewIcon` signal or a second client's `RegisterStatusNotifierItem`
    /// arriving while we're blocked on a property `Get` reply would vanish
    /// instead of reaching `recv_message` -- the tray item would flash in
    /// once and then never update again, since its very next update was the
    /// one eaten. Queueing here and having `recv_one_message` drain it first
    /// restores FIFO order across both call sites sharing one socket.
    pending: std::collections::VecDeque<Reply>,
}

impl DbusConn {
    /// Connect to the well-known system bus socket
    /// (`/var/run/dbus/system_bus_socket` — the standard path on every
    /// mainstream distro; there's no environment variable for it, unlike the
    /// session bus).
    pub fn system() -> Result<Self, DbusError> {
        Self::connect_path(b"/var/run/dbus/system_bus_socket")
    }

    /// Connect to the session bus at wherever [`SessionAddress`] resolves it
    /// -- `DBUS_SESSION_BUS_ADDRESS` when the environment provides it, else
    /// the well-known `$XDG_RUNTIME_DIR/bus` socket.
    ///
    /// The fallback matters for processes spawned by a Wayland compositor:
    /// they inherit the compositor's environment, which frequently lacks the
    /// variable entirely even though the user bus is running normally. See
    /// [`SessionAddress::resolve`] for the full rationale.
    pub fn session() -> Result<Self, DbusError> {
        match SessionAddress::resolve().ok_or(DbusError::NoAddress)? {
            SessionAddress::Path(path) => Self::connect_path(path.as_bytes()),
            SessionAddress::Abstract(name) => Self::connect_abstract(name.as_bytes()),
        }
    }

    fn connect_path(path: &[u8]) -> Result<Self, DbusError> {
        Self::connect_addr(&sys::SockAddrUn::new(path))
    }

    fn connect_abstract(name: &[u8]) -> Result<Self, DbusError> {
        Self::connect_addr(&sys::SockAddrUn::new_abstract(name))
    }

    fn connect_addr(addr: &sys::SockAddrUn) -> Result<Self, DbusError> {
        let sock = sys::DbusSys::make_unix_socket().map_err(DbusError::Connect)?;
        let sock_fd = sys::DbusSys::as_uring_fd(&sock);

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
            pending: std::collections::VecDeque::new(),
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

    /// Send an already-framed server reply. Kept narrow instead of exposing
    /// raw socket access: callers can answer an incoming D-Bus method call
    /// without being able to violate this connection's serial bookkeeping.
    pub fn send_method_return(
        &mut self,
        destination: &str,
        call_serial: u32,
        args: &[BodyArg],
    ) -> Result<(), DbusError> {
        let serial = self.take_serial();
        let frame = crate::message::MessageBuilder::build_method_return(
            serial,
            destination,
            call_serial,
            args,
        );
        self.send_raw(&frame)
    }

    /// Answer an incoming method call with an arbitrary reply body (e.g. a
    /// portal method's `a{sv}` result, or anything else
    /// [`crate::message::BodyArg`]'s fixed set can't express). The
    /// generic sibling of [`Self::send_method_return`], needed once a
    /// server side must answer with more than the handful of shapes that
    /// method covers.
    ///
    /// Returns [`DbusError::Malformed`] if `values` doesn't match
    /// `reply_signature` -- the same mismatch
    /// [`crate::marshal::Marshaller::marshal_body`] reports, surfaced as a
    /// connection error since sending a malformed frame isn't recoverable
    /// by retrying at this layer.
    pub fn send_method_return_with_values(
        &mut self,
        destination: &str,
        call_serial: u32,
        values: &[crate::value::Value],
        reply_signature: &str,
    ) -> Result<(), DbusError> {
        let serial = self.take_serial();
        let frame = crate::message::MessageBuilder::build_method_return_with_values(
            serial,
            destination,
            call_serial,
            values,
            reply_signature,
        )
        .ok_or(DbusError::Malformed)?;
        self.send_raw(&frame)
    }

    /// Answer an incoming method call with an `Error` reply -- the shape
    /// [`Self::call`] itself expects back for a failed call, and what a
    /// server side must send instead of a method return when dispatch
    /// fails (a `dbus-server` `DispatchError`, or a portal method
    /// rejecting its own arguments at the application level).
    pub fn send_error(
        &mut self,
        destination: &str,
        call_serial: u32,
        error_name: &str,
        message: &str,
    ) -> Result<(), DbusError> {
        let serial = self.take_serial();
        let frame = crate::message::MessageBuilder::build_error(
            serial,
            destination,
            call_serial,
            error_name,
            message,
        );
        self.send_raw(&frame)
    }

    /// Broadcast a signal carrying an arbitrary body -- the generic
    /// sibling of [`Self::send_signal`], needed for e.g.
    /// `Properties.PropertiesChanged`'s `(s, a{sv}, as)` shape.
    ///
    /// Returns [`DbusError::Malformed`] on the same signature/value
    /// mismatch [`Self::send_method_return_with_values`] does.
    pub fn send_signal_with_values(
        &mut self,
        path: &str,
        interface: &str,
        member: &str,
        values: &[crate::value::Value],
        signature: &str,
    ) -> Result<(), DbusError> {
        let serial = self.take_serial();
        let frame = crate::message::MessageBuilder::build_signal_with_values(
            serial, path, interface, member, values, signature,
        )
        .ok_or(DbusError::Malformed)?;
        self.send_raw(&frame)
    }

    /// Broadcast a signal from one of the D-Bus objects this connection
    /// serves. `StatusNotifier` clients use `StatusNotifierHostRegistered` as
    /// the authoritative transition from "a watcher name exists" to "there
    /// is a live visual host", so a watcher must publish it after claiming
    /// its name instead of merely answering a later property probe.
    pub fn send_signal(
        &mut self,
        path: &str,
        interface: &str,
        member: &str,
        args: &[BodyArg],
    ) -> Result<(), DbusError> {
        let frame = crate::message::MessageBuilder::build_signal(
            self.take_serial(),
            path,
            interface,
            member,
            args,
        );
        self.send_raw(&frame)
    }

    fn take_serial(&mut self) -> u32 {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.wrapping_add(1).max(1);
        serial
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
        let uid = sys::DbusSys::effective_uid();
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

    /// Make a method call whose argument body needs an arbitrary
    /// `Value` shape (e.g. a portal method's `as` or `a{sv}` argument)
    /// rather than the handful `BodyArg` can express -- the generic
    /// sibling of `Self::call`. Returns `DbusError::Malformed` if
    /// `values` doesn't match `signature`, the same mismatch
    /// `crate::marshal::Marshaller::marshal_body` reports.
    pub fn call_with_values(
        &mut self,
        destination: &str,
        path: &str,
        interface: &str,
        member: &str,
        values: &[crate::value::Value],
        signature: &str,
    ) -> Result<Reply, DbusError> {
        let serial = self.take_serial();
        let msg = crate::message::MessageBuilder::build_method_call_with_values(
            serial,
            destination,
            path,
            interface,
            member,
            values,
            signature,
        )
        .ok_or(DbusError::Malformed)?;
        self.send_raw(&msg)?;
        let reply = self.await_reply(serial)?;
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
        let serial = self.take_serial();
        let msg = crate::message::MessageBuilder::build_method_call(
            serial,
            destination,
            path,
            interface,
            member,
            args,
        );
        self.send_raw(&msg)?;
        self.await_reply(serial)
    }

    /// Block until the reply matching `serial` arrives, queueing any
    /// unsolicited signal or unrelated reply encountered along the way --
    /// shared by `Self::call_raw` and `Self::call_with_values`, the two
    /// frame-sending paths that both need to wait out the same
    /// interleaved-signal noise.
    fn await_reply(&mut self, serial: u32) -> Result<Reply, DbusError> {
        // A call made from inside the tray watcher's dispatch loop (e.g.
        // `get_property` while handling a registration) can nest inside an
        // outer `recv_message` that already left unrelated messages queued.
        // This call's own reply could in principle already be sitting among
        // them (unlikely given the request was only just sent, but cheap to
        // check and keeps the invariant simple): pull it out in place,
        // leaving everything else queued in its original order.
        if let Some(index) = self
            .pending
            .iter()
            .position(|reply| reply.reply_serial == Some(serial))
        {
            // `swap_remove_back` would reorder the queue; a plain `remove`
            // keeps every other pending message's relative order intact.
            if let Some(reply) = self.pending.remove(index) {
                return Ok(reply);
            }
        }

        // The bus can (and does) interleave unsolicited signals with the
        // reply we're waiting for -- e.g. `NameAcquired` lands right after
        // `Hello`, and any service can broadcast `PropertiesChanged` at any
        // time. Reading exactly one message and assuming it's the reply
        // silently desyncs every call after the first stray signal (every
        // later "reply" is actually the previous call's real reply, or a
        // different signal, shifted by one). Loop, queuing anything whose
        // `REPLY_SERIAL` doesn't match this call's serial instead of
        // dropping it, so `recv_message` can still dispatch it later.
        loop {
            let reply = self.recv_wire_message()?;
            if reply.reply_serial == Some(serial) {
                return Ok(reply);
            }
            self.pending.push_back(reply);
        }
    }

    /// Read one complete message frame off the wire (prelude, then exactly
    /// as many more bytes as the prelude says), regardless of what kind of
    /// message it turns out to be. Bypasses the pending queue -- this is the
    /// only place that actually reads the socket; every other reader goes
    /// through `recv_one_message`, which checks `pending` first.
    fn recv_wire_message(&mut self) -> Result<Reply, DbusError> {
        let prelude_bytes = self.recv_exact(PRELUDE_LEN)?;
        let prelude = Prelude::parse(&prelude_bytes).ok_or(DbusError::Malformed)?;
        let remaining = prelude.total_len() - PRELUDE_LEN;
        let rest = self.recv_exact(remaining)?;

        let mut full = prelude_bytes;
        full.extend_from_slice(&rest);
        Reply::parse_message(&full).ok_or(DbusError::Malformed)
    }

    /// Claim a well-known session-bus name.
    ///
    /// A `StatusNotifier` watcher must own
    /// `org.kde.StatusNotifierWatcher` before tray clients will register
    /// their items with it. `PrimaryOwner` is deliberately requested instead
    /// of queueing: a second bar must fail visibly rather than becoming a
    /// silent standby watcher that never receives registrations.
    pub fn request_name(&mut self, name: &str) -> Result<bool, DbusError> {
        const DBUS: &str = "org.freedesktop.DBus";
        const PATH: &str = "/org/freedesktop/DBus";
        const PRIMARY_OWNER: u32 = 0x1;
        const PRIMARY_OWNER_REPLY: u64 = 1;
        let reply = self.call(
            DBUS,
            PATH,
            DBUS,
            "RequestName",
            &[BodyArg::Str(name.to_owned()), BodyArg::U32(PRIMARY_OWNER)],
        )?;
        Ok(reply.values().first().and_then(crate::value::Value::as_u64)
            == Some(PRIMARY_OWNER_REPLY))
    }

    /// Wait for the next complete incoming message. A `StatusNotifier` watcher
    /// uses this server-side half of the connection to receive item
    /// registrations and property-change signals between normal polling
    /// calls. Unlike [`Self::call`], it intentionally does not discard
    /// messages based on reply serial.
    ///
    /// Checks `pending` first: a `call()` made from inside the watcher's
    /// dispatch loop (e.g. `get_property` while handling a registration) may
    /// have already read a later signal or method call off the wire while
    /// waiting for its own reply and queued it rather than dropping it, so
    /// this must drain that queue in order before touching the socket again.
    pub fn recv_message(&mut self) -> Result<Reply, DbusError> {
        self.recv_one_message()
    }

    /// The underlying socket's raw file descriptor, for polling alongside
    /// another fd via a plain `poll(2)` (e.g. `waker::Poller`) -- a server
    /// that must react to *both* incoming bus messages and some other
    /// event source (`bins/portal`'s config-file inotify watch, say)
    /// cannot just block forever in [`Self::recv_message`]'s blocking
    /// `io_uring` recv; polling this fd first (with the other source)
    /// tells the caller a message is actually available before it
    /// blocks here.
    ///
    /// Deliberately doesn't consult `pending` -- today's only caller
    /// (`bins/portal`) never nests calls inside its own receive loop the
    /// way the tray watcher does, so `pending` is always empty for it in
    /// practice; a future caller that does nest calls would need its own
    /// accessor for that queue's readiness.
    #[must_use]
    pub const fn raw_fd(&self) -> i32 {
        self.sock_fd.as_i32()
    }

    /// Pop the next message, preferring anything already queued by a prior
    /// `call_raw` over reading fresh bytes off the wire, to preserve FIFO
    /// order across both call sites sharing one socket.
    fn recv_one_message(&mut self) -> Result<Reply, DbusError> {
        if let Some(reply) = self.pending.pop_front() {
            return Ok(reply);
        }
        self.recv_wire_message()
    }

    /// Subscribe this connection to messages matching a D-Bus match rule.
    ///
    /// The bus does not forward broadcast signals merely because a client can
    /// parse them; a `StatusNotifier` watcher must explicitly subscribe before
    /// `NewIcon` and `NewStatus` can reach its receive loop.
    pub fn add_match(&mut self, rule: &str) -> Result<(), DbusError> {
        self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            &[BodyArg::Str(rule.to_owned())],
        )?;
        Ok(())
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
