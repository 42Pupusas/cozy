//! `portal` — this workspace's native `org.freedesktop.impl.portal.*`
//! backend daemon.
//!
//! Per `docs/portal-scoping.md` step 5: the first (and today, only)
//! interface wired up is `org.freedesktop.impl.portal.Settings`, backed by
//! [`portal_core::AppearanceSettings`]. As with `bins/clipboard`/
//! `bins/shot`, all the interesting logic lives elsewhere -- `dbus-server`
//! owns generic object/method/property dispatch, `portal-core` owns this
//! interface's domain logic, [`PortalLoop`] owns pumping both the bus and
//! the config-file watch -- so this binary is wiring only: connect, claim
//! a name, run the loop.
//!
//! Claims [`SettingsService::NAME`], the real activation name
//! `bins/portal/data/tijaax.portal` advertises -- see that file and
//! `bins/portal/data/tijaax-portal.service.in` for the rest of step 5's
//! "ship a real `.portal` file and route it" half.

use dbus::conn::DbusConn;
use portal_bin::{PortalLoop, SettingsService, SettingsState};

fn main() {
    let cfg = config::Config::load().unwrap_or_else(|e| {
        eprintln!("portal: config error: {e} — using defaults");
        config::Config::default()
    });
    let state = SettingsState::new(&cfg);

    let conn = match DbusConn::session() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("portal: could not connect to the session bus: {e}");
            std::process::exit(1);
        }
    };

    let mut portal_loop = PortalLoop::new(conn, state);
    match portal_loop.conn_mut().request_name(SettingsService::NAME) {
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
    let err = portal_loop.run_forever();
    eprintln!("portal: stopped serving: {err}");
    std::process::exit(1);
}
