//! Probe: the session bus must be reachable *without* inheriting
//! `DBUS_SESSION_BUS_ADDRESS`.
//!
//! This is the exact condition a compositor-spawned process faces. niri's
//! `spawn-at-startup` children inherit niri's environment, which on a TTY or
//! non-`dbus-run-session` login lacks that variable entirely -- so the status
//! bar's tray watcher bailed out before claiming
//! `org.kde.StatusNotifierWatcher`, and tray clients starting alongside it
//! found no host. A unit test cannot catch this: it only shows up when the
//! variable is absent but a real user bus is running.

use dbus::SessionAddress;

/// The environment-independent fallback used by compositor children.
struct RuntimeBusProbe;

impl RuntimeBusProbe {
    /// Resolve the bus purely from `XDG_RUNTIME_DIR`, ignoring any
    /// `DBUS_SESSION_BUS_ADDRESS` this test process happens to have.
    fn resolve_without_env() -> Option<SessionAddress> {
        SessionAddress::from_runtime_dir(std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
    }
}

#[test]
fn session_bus_is_reachable_without_the_address_variable() {
    let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") else {
        eprintln!("no XDG_RUNTIME_DIR (CI sandbox?); skipping");
        return;
    };
    let Some(address) = RuntimeBusProbe::resolve_without_env() else {
        // No user bus on this machine -- nothing to assert, and failing here
        // would break builds in containers that legitimately have no bus.
        eprintln!("no user bus under {runtime_dir}; skipping");
        return;
    };
    assert_eq!(
        address,
        SessionAddress::Path(format!("{runtime_dir}/bus")),
        "the fallback must point at the well-known user bus socket"
    );
    // The whole point: with the variable stripped, resolution still succeeds.
    assert!(
        matches!(address, SessionAddress::Path(ref p) if std::fs::metadata(p).is_ok()),
        "resolved socket must actually exist"
    );
}
