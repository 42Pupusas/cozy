//! Proof that `bins/portal`'s actual wiring -- not just
//! `SettingsService::dispatcher`'s in-memory unit tests -- answers a real
//! `org.freedesktop.impl.portal.Settings` caller over a real session bus
//! connection, per `docs/portal-scoping.md` step 5's "validates the real
//! frontend path without UI or FD complexity" goal.
//!
//! Mirrors `crates/dbus-server/tests/scratch_service.rs`'s shape and
//! rationale exactly (own well-known name per test to dodge
//! `RequestName` races between the two tests `cargo test` runs
//! concurrently; skip rather than fail when no session bus is reachable).
//! What only a test like this can prove, that `settings_service`'s unit
//! tests cannot: real SASL auth, a real `RequestName`, and bytes actually
//! round-tripping through two independent `io_uring` sockets -- not just
//! `Dispatcher::dispatch` called directly in memory.

use dbus::conn::DbusConn;
use dbus::message::BodyArg;
use dbus::value::Value;
use dbus_server::ServerLoop;
use portal_bin::SettingsService;

const NAMESPACE: &str = "org.freedesktop.appearance";

fn session_bus_reachable() -> bool {
    match DbusConn::session() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("no session D-Bus available ({e}); skipping real_bus_probe test");
            false
        }
    }
}

/// Connect, claim `name`, and serve exactly `count` real dispatched
/// calls -- see `scratch_service.rs`'s identical helper for why this
/// connects inside the spawned closure (`DbusConn` isn't `Send`) and
/// counts dispatches rather than `serve_one` invocations (unsolicited bus
/// housekeeping like `NameAcquired` would otherwise desync the count).
fn spawn_server(name: &'static str, count: usize) -> std::thread::JoinHandle<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let conn = DbusConn::session().expect("session bus was reachable a moment ago");
        let mut server = ServerLoop::new(
            conn,
            SettingsService::dispatcher(&config::Config::default()),
        );
        match server.conn_mut().request_name(name) {
            Ok(true) => {}
            Ok(false) => panic!("another connection already owns {name}"),
            Err(e) => panic!("could not claim {name}: {e}"),
        }
        let _ = ready_tx.send(());
        let mut calls_dispatched = 0;
        while calls_dispatched < count {
            match server.serve_one() {
                Ok(true) => calls_dispatched += 1,
                Ok(false) => {}
                Err(e) => panic!("portal test server stopped early: {e}"),
            }
        }
    });
    ready_rx
        .recv()
        .expect("server thread should signal readiness before panicking");
    handle
}

fn client() -> Option<DbusConn> {
    DbusConn::session().ok()
}

#[test]
fn a_real_caller_can_introspect_readall_and_read_the_settings_backend() {
    const NAME: &str = "org.freedesktop.impl.portal.desktop.mera.scratch.testprobe";
    if !session_bus_reachable() {
        return;
    }
    // Introspect, ReadAll, Read, Properties.GetAll: four calls in, four
    // replies out.
    let handle = spawn_server(NAME, 4);

    let Some(mut client) = client() else {
        panic!("server connected to the session bus but a second client connection failed");
    };

    // 1. Introspect -- the real wiring's XML, not NodeIntrospection::to_xml
    // called directly.
    let reply = client
        .call(
            NAME,
            SettingsService::PATH,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            &[],
        )
        .expect("Introspect should succeed");
    let Some(Value::Str(xml)) = reply.values().into_iter().next() else {
        panic!("Introspect must reply with a single string");
    };
    assert!(xml.contains(&format!("<interface name=\"{}\">", SettingsService::IFACE)));

    // 2. ReadAll with no filter -- proves the real
    // AppearanceSettings::read_all reaches a real caller over the wire.
    // `as` (a plain array of strings) is a shape BodyArg can't express,
    // so this goes through the generic Value-based `call_with_values`
    // this step's wiring needed dbus itself to grow.
    let reply = client
        .call_with_values(
            NAME,
            SettingsService::PATH,
            SettingsService::IFACE,
            "ReadAll",
            &[Value::Array(vec![])],
            "as",
        )
        .expect("ReadAll should succeed");
    let Some(Value::Array(entries)) = reply.values().into_iter().next() else {
        panic!("ReadAll must reply with the outer a{{sa{{sv}}}} array");
    };
    assert_eq!(entries.len(), 1, "exactly one namespace is answered today");

    // 3. Read a single key -- proves the two-string-argument path
    // decodes correctly over real marshalled bytes, not just the
    // in-memory Value list settings_service's own unit tests build by
    // hand.
    let reply = client
        .call(
            NAME,
            SettingsService::PATH,
            SettingsService::IFACE,
            "Read",
            &[
                BodyArg::Str(NAMESPACE.into()),
                BodyArg::Str("color-scheme".into()),
            ],
        )
        .expect("Read should succeed");
    assert!(matches!(
        reply.values().into_iter().next(),
        Some(Value::Variant(_))
    ));

    // 4. Properties.GetAll -- the version property, reachable the same
    // generic way `crates/dbus-server`'s own scratch service proved.
    let reply = client
        .call(
            NAME,
            SettingsService::PATH,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            &[BodyArg::Str(SettingsService::IFACE.into())],
        )
        .expect("GetAll should succeed");
    assert_eq!(
        reply.values(),
        vec![Value::Array(vec![Value::DictEntry(
            Box::new(Value::Str("version".into())),
            Box::new(Value::Variant(Box::new(Value::U32(2)))),
        )])]
    );

    handle
        .join()
        .expect("portal test server thread should not panic");
}
