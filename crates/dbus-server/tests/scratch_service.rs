//! Proof that `dbus-server`'s pieces work together over a **real** D-Bus
//! session bus connection, per `docs/portal-scoping.md` step 4: a small
//! scratch service, on a scratch well-known name, exercised by a second,
//! wholly independent client connection -- introspection, a method call,
//! `GetAll`, malformed arguments, and a signal -- before any real portal
//! interface is ever registered.
//!
//! This intentionally duplicates nothing from the unit tests scattered
//! through `src/*.rs`: those already prove `Dispatcher::dispatch` and
//! `IncomingCall::serve` correct against in-memory registries/fakes. What
//! only an integration test like this one can prove is that the *wire*
//! path works end to end: real SASL auth, a real `RequestName`, bytes
//! genuinely round-tripping through two independent `io_uring` sockets,
//! not just through `Marshaller`/`Unmarshaller` in memory.
//!
//! Skips (rather than failing) if no session bus is reachable, matching
//! `crates/dbus`'s own `session_address_probe` integration test -- CI
//! sandboxes and containers legitimately have none.
//!
//! Each test claims its *own* well-known name (`SCRATCH_NAME_*`) rather
//! than sharing one: `cargo test` runs both tests in this file
//! concurrently by default, and two connections racing to
//! `RequestName(..., PrimaryOwner)` the same name would make one of them
//! fail non-deterministically -- not a real bug, just cross-test
//! interference this file must not manufacture.

use dbus::conn::{DbusConn, DbusError};
use dbus::message::Reply;
use dbus::value::Value;
use dbus_server::{Dispatcher, InterfaceDescriptor, ServerLoop};

const SCRATCH_PATH: &str = "/org/freedesktop/dbusserver/Scratch";
const SCRATCH_IFACE: &str = "org.freedesktop.dbusserver.Scratch1";

/// Build the scratch service's `Dispatcher`: one object implementing one
/// business interface with a real method (`Echo`, returns its argument
/// unchanged -- simple enough to leave no doubt a mismatch is this test's
/// bug and not the method's), plus one property so `GetAll` has
/// something real to return.
fn scratch_dispatcher() -> Dispatcher {
    let mut dispatcher = Dispatcher::new();
    dispatcher.register_object(SCRATCH_PATH, [SCRATCH_IFACE]);
    dispatcher.register_interface(InterfaceDescriptor::new(SCRATCH_IFACE));
    dispatcher.register_method(SCRATCH_IFACE, "Echo", "s", "s", |_path, args| {
        let Some(Value::Str(text)) = args.first() else {
            return Err("Echo needs one string argument".to_string());
        };
        Ok(vec![Value::Str(text.clone())])
    });
    dispatcher.set_property(
        SCRATCH_PATH,
        SCRATCH_IFACE,
        "greeting",
        Value::Str("hello scratch".into()),
    );
    dispatcher
}

/// Whether a session bus is reachable at all -- checked from the test
/// thread before spawning the server thread, so a "no bus here" skip
/// can return cleanly from the `#[test]` fn itself rather than requiring
/// the spawned thread to signal it back.
fn session_bus_reachable() -> bool {
    match DbusConn::session() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("no session D-Bus available ({e}); skipping scratch service test");
            false
        }
    }
}

/// Connect, claim `name`, and serve exactly `count` messages, all on the
/// calling thread -- `ququmatz::IoUring` (and so `DbusConn`) isn't
/// `Send`, by design: an `io_uring` instance is meant to stay pinned to
/// the thread that submits to it, the same way `platform::conn`'s
/// `Sender`/`Reader` each own their own ring rather than sharing one
/// across threads. So this connects *inside* the spawned closure instead
/// of building a `DbusConn` on the test thread and moving it in.
///
/// Blocks the *caller* until `name` is actually claimed, via a rendezvous
/// channel -- without this, the test thread could call before
/// `RequestName` completes and see a spurious `ServiceUnknown` instead of
/// exercising the scratch service at all.
fn spawn_server(name: &'static str, count: usize) -> std::thread::JoinHandle<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let conn = DbusConn::session().expect("session bus was reachable a moment ago");
        let mut server = ServerLoop::new(conn, scratch_dispatcher());
        match server.conn_mut().request_name(name) {
            Ok(true) => {}
            Ok(false) => panic!("another connection already owns {name}"),
            Err(e) => panic!("could not claim {name}: {e}"),
        }
        let _ = ready_tx.send(());
        // Loop on real dispatched calls, not on `serve_one` invocations:
        // this connection also receives unsolicited bus housekeeping
        // (e.g. `NameAcquired` right after `RequestName`), which
        // `serve_one` correctly reports as `Ok(false)` rather than
        // dispatching -- counting invocations instead of dispatches would
        // stop this loop one message early and desync the whole exchange
        // with the client.
        let mut calls_dispatched = 0;
        while calls_dispatched < count {
            match server.serve_one() {
                Ok(true) => calls_dispatched += 1,
                Ok(false) => {}
                Err(e) => panic!("scratch server stopped early: {e}"),
            }
        }
    });
    ready_rx
        .recv()
        .expect("server thread should signal readiness before panicking");
    handle
}

/// A fresh client connection to the session bus, or `None` on the same
/// "no bus reachable" condition the server side already checked (kept
/// separate in case the two calls ever diverge, but expected to agree).
fn client() -> Option<DbusConn> {
    DbusConn::session().ok()
}

/// Read messages off `conn` until one matches `(interface, member)`,
/// discarding everything else along the way.
///
/// A freshly connected client's very first messages are always bus
/// housekeeping (`NameAcquired` for its own unique name, etc.), not
/// whatever this test is waiting for -- `recv_message` alone would hand
/// back the *first* thing off the wire, which is one of those, not the
/// scratch service's own signal. Real services (`statusbar::tray`'s
/// receive loop, most concretely) always filter incoming messages this
/// way rather than assuming positional order.
fn recv_until(conn: &mut DbusConn, interface: &str, member: &str) -> Reply {
    loop {
        let message = conn.recv_message().expect("recv_message should succeed");
        if message.interface.as_deref() == Some(interface)
            && message.member.as_deref() == Some(member)
        {
            return message;
        }
    }
}

#[test]
fn scratch_service_answers_introspect_a_method_call_getall_and_bad_args() {
    const NAME: &str = "org.freedesktop.dbusserver.ScratchCalls";
    if !session_bus_reachable() {
        return;
    }
    // Every exchange below is exactly one method call, so five calls in,
    // five replies out: Introspect, Echo, GetAll, one deliberately
    // malformed Echo, and Get on a missing property.
    let handle = spawn_server(NAME, 5);

    let Some(mut client) = client() else {
        panic!("server connected to the session bus but a second client connection failed");
    };

    // 1. Introspect -- proves the wire path renders real XML naming the
    // scratch interface, not just NodeIntrospection::to_xml in memory.
    let reply = client
        .call(
            NAME,
            SCRATCH_PATH,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            &[],
        )
        .expect("Introspect should succeed");
    let values = reply.values();
    let Some(Value::Str(xml)) = values.first() else {
        panic!("Introspect must reply with a single string, got {values:?}");
    };
    assert!(
        xml.contains(&format!("<interface name=\"{SCRATCH_IFACE}\">")),
        "introspection XML should name the scratch interface:\n{xml}"
    );
    assert!(xml.contains("<interface name=\"org.freedesktop.DBus.Properties\">"));

    // 2. A real business method call, round-tripped through two
    // independent io_uring sockets and real SASL-authenticated
    // connections -- not just Dispatcher::dispatch in memory.
    let reply = client
        .call(
            NAME,
            SCRATCH_PATH,
            SCRATCH_IFACE,
            "Echo",
            &[dbus::message::BodyArg::Str("hello over the wire".into())],
        )
        .expect("Echo should succeed");
    assert_eq!(
        reply.values(),
        vec![Value::Str("hello over the wire".into())]
    );

    // 3. Properties.GetAll -- proves the generic Properties handling in
    // Dispatcher reaches a real caller, not just the unit tests that
    // call `dispatcher.dispatch(...)` directly.
    let reply = client
        .call(
            NAME,
            SCRATCH_PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            &[dbus::message::BodyArg::Str(SCRATCH_IFACE.into())],
        )
        .expect("GetAll should succeed");
    assert_eq!(
        reply.values(),
        vec![Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str("greeting".into())),
            Box::new(Value::Variant(Box::new(Value::Str("hello scratch".into())))),
        )])]
    );

    // 4. Malformed arguments (wrong signature: `Echo` expects `s`, this
    // sends nothing) -- proves a bad call comes back as a real wire
    // `Error` message, not a hang or a connection drop.
    let err = client
        .call(NAME, SCRATCH_PATH, SCRATCH_IFACE, "Echo", &[])
        .expect_err("Echo with no arguments should fail");
    let DbusError::Reply { name } = err else {
        panic!("expected a D-Bus error reply, got {err}");
    };
    assert_eq!(name, "org.freedesktop.DBus.Error.InvalidArgs");

    // 5. Get on a property that was never set -- the "Failed" branch,
    // distinct from the structural mismatches above.
    let err = client
        .call(
            NAME,
            SCRATCH_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            &[
                dbus::message::BodyArg::Str(SCRATCH_IFACE.into()),
                dbus::message::BodyArg::Str("does-not-exist".into()),
            ],
        )
        .expect_err("Get on a missing property should fail");
    let DbusError::Reply { name } = err else {
        panic!("expected a D-Bus error reply, got {err}");
    };
    assert_eq!(name, "org.freedesktop.DBus.Error.Failed");

    handle
        .join()
        .expect("scratch server thread should not panic");
}

#[test]
fn scratch_service_emits_a_signal_an_external_client_can_receive() {
    const NAME: &str = "org.freedesktop.dbusserver.ScratchSignal";
    if !session_bus_reachable() {
        return;
    }

    // Rendezvous in both directions: the server signals once it has
    // claimed the name, and the client signals once it has subscribed
    // (so the signal isn't emitted before anyone is listening for it) --
    // the bus does not buffer broadcasts for latecomers.
    let (server_ready_tx, server_ready_rx) = std::sync::mpsc::channel::<()>();
    let (client_subscribed_tx, client_subscribed_rx) = std::sync::mpsc::channel::<()>();

    let handle = std::thread::spawn(move || {
        let conn = DbusConn::session().expect("session bus was reachable a moment ago");
        let mut server = ServerLoop::new(conn, scratch_dispatcher());
        match server.conn_mut().request_name(NAME) {
            Ok(true) => {}
            Ok(false) => panic!("another connection already owns {NAME}"),
            Err(e) => panic!("could not claim {NAME}: {e}"),
        }
        let _ = server_ready_tx.send(());
        client_subscribed_rx
            .recv()
            .expect("test thread should signal once subscribed");

        // Emit the signal directly on the server's connection (not
        // through Dispatcher -- signals are an out-of-band broadcast, not
        // a method reply, so there's nothing for the dispatcher to
        // decide here).
        server
            .conn_mut()
            .send_signal(
                SCRATCH_PATH,
                SCRATCH_IFACE,
                "Greeted",
                &[dbus::message::BodyArg::Str("hello signal".into())],
            )
            .expect("send_signal should succeed");
    });

    server_ready_rx
        .recv()
        .expect("server thread should signal readiness before panicking");

    let Some(mut client) = client() else {
        panic!("server connected to the session bus but a second client connection failed");
    };
    // Subscribe before the signal is sent -- the bus only forwards
    // broadcasts to connections that asked for them, exactly like
    // `statusbar::tray`'s watcher must `add_match` before `NewIcon`
    // signals reach it.
    client
        .add_match(&format!("type='signal',interface='{SCRATCH_IFACE}'"))
        .expect("add_match should succeed");
    let _ = client_subscribed_tx.send(());

    // Skip past whatever bus housekeeping (e.g. this connection's own
    // `NameAcquired`) arrives ahead of the scratch service's signal.
    let message = recv_until(&mut client, SCRATCH_IFACE, "Greeted");
    assert_eq!(message.values(), vec![Value::Str("hello signal".into())]);

    handle
        .join()
        .expect("scratch server thread should not panic");
}
