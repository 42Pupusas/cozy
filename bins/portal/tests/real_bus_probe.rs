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
use portal_bin::{ConfigWatcher, PortalLoop, SettingsService, SettingsState};

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
        let state = SettingsState::new(&config::Config::default());
        let mut server = ServerLoop::new(conn, SettingsService::dispatcher(state));
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

/// Proof of the other half of `docs/portal-scoping.md` step 5's
/// remaining work: a config-file save reaching a real, second,
/// independently-connected client as a real `SettingChanged` signal --
/// not just `SettingsState::reload`'s in-memory unit tests (which prove
/// the value comparison, but never touch a filesystem, `inotify`, or a
/// bus connection).
#[test]
fn a_config_save_emits_a_real_setting_changed_signal_to_a_subscribed_client() {
    const NAME: &str = "org.freedesktop.impl.portal.desktop.tijaax.testreload";
    if !session_bus_reachable() {
        return;
    }

    let dir = std::env::temp_dir().join(format!(
        "portal-real-bus-reload-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let config_path = dir.join("config.toml");
    let initial_toml = b"[files]\nbackground = \"#181818\"\n";
    std::fs::write(&config_path, initial_toml).unwrap();
    let initial_cfg = config::Config::from_bytes(initial_toml).expect("parse initial config");

    let (server_ready_tx, server_ready_rx) = std::sync::mpsc::channel::<()>();
    let (client_subscribed_tx, client_subscribed_rx) = std::sync::mpsc::channel::<()>();
    let config_path_for_server = config_path.clone();

    let handle = std::thread::spawn(move || {
        let conn = DbusConn::session().expect("session bus was reachable a moment ago");
        let state = SettingsState::new(&initial_cfg);
        let watcher = ConfigWatcher::for_path(config_path_for_server);
        let mut portal_loop = PortalLoop::with_watcher(conn, state, watcher);
        match portal_loop.conn_mut().request_name(NAME) {
            Ok(true) => {}
            Ok(false) => panic!("another connection already owns {NAME}"),
            Err(e) => panic!("could not claim {NAME}: {e}"),
        }
        let _ = server_ready_tx.send(());
        client_subscribed_rx
            .recv()
            .expect("test thread should signal once subscribed");

        // Exactly one iteration: the config-file write below is this
        // test's only event, and `pump_once`'s poll blocks (rather than
        // busy-spins) until it actually happens, so there is no race to
        // sequence against the write below.
        portal_loop
            .pump_n(1)
            .expect("pump_n should not hit a transport error");
    });

    server_ready_rx
        .recv()
        .expect("server thread should signal readiness before panicking");

    let Some(mut client) = client() else {
        panic!("server connected to the session bus but a second client connection failed");
    };
    client
        .add_match(&format!(
            "type='signal',interface='{}'",
            SettingsService::IFACE
        ))
        .expect("add_match should succeed");
    let _ = client_subscribed_tx.send(());

    // A background crossing the dark/light threshold -- the one
    // `AppearanceSettings` key with an unconditional, deterministic
    // mapping (`ColorScheme::from_background`), so this write is
    // guaranteed to actually change something ReadAll answers.
    std::fs::write(&config_path, b"[files]\nbackground = \"#f8f8f8\"\n").unwrap();

    let message = recv_until(&mut client, SettingsService::IFACE, "SettingChanged");
    let values = message.values();
    assert_eq!(values.len(), 3, "SettingChanged is (namespace, key, value)");
    assert_eq!(values[0], Value::Str(NAMESPACE.into()));
    assert_eq!(values[1], Value::Str("color-scheme".into()));
    assert!(matches!(values[2], Value::Variant(_)));

    handle
        .join()
        .expect("portal reload test server thread should not panic");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Read messages off `conn` until one matches `(interface, member)`,
/// discarding everything else along the way -- the same filtering
/// `crates/dbus-server`'s scratch-service test needs, for the same
/// reason (a fresh connection's first messages are bus housekeeping, not
/// whatever this test is waiting for).
fn recv_until(conn: &mut DbusConn, interface: &str, member: &str) -> dbus::message::Reply {
    loop {
        let message = conn.recv_message().expect("recv_message should succeed");
        if message.interface.as_deref() == Some(interface)
            && message.member.as_deref() == Some(member)
        {
            return message;
        }
    }
}
