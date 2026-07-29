//! The blocking receive loop a real D-Bus service runs: pull one message
//! off a [`DbusConn`], and if it's a method call, dispatch it through a
//! [`Dispatcher`] and answer it.
//!
//! Deliberately thin -- this is the last piece [`crate::serve`] needs a
//! concrete connection for, not a place to grow policy. Everything
//! testable without a real socket already has its own unit tests
//! ([`crate::dispatcher::Dispatcher::dispatch`],
//! [`crate::serve::IncomingCall::serve`] against a fake
//! [`crate::serve::ReplySink`]); [`ServerLoop`] itself is proven by the
//! scratch-service integration test that talks to it over a real session
//! bus connection from a second, independent client, per this project's
//! delivery plan (dbus-server's own doc comment / `docs/portal-scoping.md`
//! step 4) -- introspection, a method call, `GetAll`, malformed arguments,
//! and a signal, all before any real portal interface is registered.
//!
//! Non-`MethodCall` messages (signals, stray replies) are silently
//! ignored here rather than an error: a real service's `NameOwnerChanged`
//! handling (via [`crate::name_watcher::NameWatcher`]) and any signals it
//! itself subscribes to are the caller's concern, layered on top by
//! matching on [`dbus::message::Reply::interface`]/`member` the same way
//! `statusbar::tray`'s receive loop already does -- this loop only
//! guarantees that whatever *is* a method call gets a reply.

use dbus::conn::{DbusConn, DbusError};

use crate::dispatcher::Dispatcher;
use crate::serve::IncomingCall;

/// A [`DbusConn`] paired with the [`Dispatcher`] that answers calls
/// arriving on it.
pub struct ServerLoop {
    conn: DbusConn,
    dispatcher: Dispatcher,
}

impl ServerLoop {
    /// Pair an already-connected `conn` (already past `request_name`, if
    /// the caller wants a well-known name -- this type doesn't own that
    /// decision either) with `dispatcher`.
    #[must_use]
    pub const fn new(conn: DbusConn, dispatcher: Dispatcher) -> Self {
        Self { conn, dispatcher }
    }

    /// Mutable access to the dispatcher, for registering objects/methods
    /// after construction (or from another thread's perspective before
    /// [`Self::run_forever`]/[`Self::serve_one`] starts consuming
    /// messages -- there's no interior synchronization, so a caller that
    /// wants to mutate registration concurrently with serving must add
    /// its own).
    pub const fn dispatcher_mut(&mut self) -> &mut Dispatcher {
        &mut self.dispatcher
    }

    /// Mutable access to the underlying connection, e.g. for `add_match`
    /// calls a caller wants to make before entering the loop.
    pub const fn conn_mut(&mut self) -> &mut DbusConn {
        &mut self.conn
    }

    /// Block for exactly one incoming message. If it's a well-formed
    /// method call, dispatch it and send the reply; anything else
    /// (malformed method call missing a routing field, non-`MethodCall`
    /// message -- including bus housekeeping like `NameAcquired`, which
    /// every connection receives unsolicited right after `Hello`/
    /// `RequestName`) is silently skipped -- see the module doc comment
    /// for why silence is correct here rather than an error.
    ///
    /// Returns `Ok(true)` if a method call was actually dispatched,
    /// `Ok(false)` if the message was something else and got skipped --
    /// a caller counting "messages served" (e.g. this crate's own
    /// scratch-service integration test, which serves a known number of
    /// real calls and must not miscount an interleaved `NameAcquired` as
    /// one of them) needs this distinction; `run_forever` doesn't care
    /// and ignores it.
    ///
    /// The `Err` case is a transport-level failure only (the connection
    /// closing, an I/O error) -- a caller's dispatch/send failure already
    /// became a wire `Error` reply to the caller that sent it, not a
    /// reason to stop serving everyone else.
    pub fn serve_one(&mut self) -> Result<bool, DbusError> {
        let message = self.conn.recv_message()?;
        let Some(call) = IncomingCall::from_reply(&message) else {
            return Ok(false);
        };
        // A failed *send* (e.g. the caller already disconnected) is a
        // transport-layer detail about that one caller, not a reason to
        // tear down the whole loop -- matching `statusbar::tray`'s
        // existing `let _ = conn.send_method_return(..)` pattern for the
        // same situation.
        let _ = call.serve(&self.dispatcher, &mut self.conn);
        Ok(true)
    }

    /// Serve forever, stopping only on a transport-level error (the
    /// connection closing). A real binary's `main` calls this directly;
    /// tests call [`Self::serve_one`] in a bounded loop instead so they
    /// can stop deterministically once they've seen what they're
    /// checking for.
    pub fn run_forever(&mut self) -> DbusError {
        loop {
            if let Err(e) = self.serve_one() {
                return e;
            }
        }
    }
}
