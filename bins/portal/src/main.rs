//! `portal` — this workspace's native `org.freedesktop.impl.portal.*`
//! backend daemon.
//!
//! Per `docs/portal-scoping.md` step 5: the first (and today, only)
//! interface wired up is `org.freedesktop.impl.portal.Settings`, backed by
//! [`portal_core::AppearanceSettings`]. As with `bins/clipboard`/
//! `bins/shot`, all the interesting logic lives elsewhere -- `dbus-server`
//! owns generic object/method/property dispatch, `portal-core` owns this
//! interface's domain logic -- so this binary is wiring only: connect,
//! claim a name, register one object, serve forever.
//!
//! Claims a **scratch** well-known name (see [`SettingsService::NAME`]'s
//! doc comment) rather than the real
//! `org.freedesktop.impl.portal.desktop.mera` a `.portal` file would name,
//! matching this project's own delivery-sequence contract (`docs/
//! portal-scoping.md` section 4: "develop every interface first on a
//! scratch bus name" before ever routing it via `portals.conf`).

use dbus::conn::DbusConn;
use dbus_server::ServerLoop;
use portal_bin::SettingsService;

fn main() {
    let cfg = config::Config::load().unwrap_or_else(|e| {
        eprintln!("portal: config error: {e} — using defaults");
        config::Config::default()
    });

    let conn = match DbusConn::session() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("portal: could not connect to the session bus: {e}");
            std::process::exit(1);
        }
    };

    let mut server = ServerLoop::new(conn, SettingsService::dispatcher(&cfg));
    match server.conn_mut().request_name(SettingsService::NAME) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "portal: {} is already owned by another connection",
                SettingsService::NAME
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("portal: could not claim {}: {e}", SettingsService::NAME);
            std::process::exit(1);
        }
    }

    eprintln!(
        "portal: serving {} on {}",
        SettingsService::IFACE,
        SettingsService::NAME
    );
    let err = server.run_forever();
    eprintln!("portal: stopped serving: {err}");
    std::process::exit(1);
}
