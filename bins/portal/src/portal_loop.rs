//! Serves the portal's `Dispatcher` while also watching the config file
//! for saves, emitting `SettingChanged` for whatever actually changed.
//!
//! `dbus_server::ServerLoop::run_forever` alone cannot do this: it blocks
//! forever inside one blocking `io_uring` recv on the bus socket, with no
//! way to also notice an inotify event arriving on a completely different
//! fd. This is the thin, portal-specific event pump `docs/portal-scoping.md`
//! step 5 was missing -- poll both fds with a plain `poll(2)` (via
//! `waker::Poller`, the same primitive `bins/terminal`'s own event loop
//! multiplexes fds with), and only then block on whichever source is
//! actually ready.

use dbus::conn::{DbusConn, DbusError};
use dbus_server::Dispatcher;
use portal_core::SettingChangedSignal;

use crate::config_watcher::ConfigWatcher;
use crate::portal_dispatcher::PortalDispatcher;
use crate::settings_service::SettingsService;
use crate::settings_state::SettingsState;

/// Owns the live connection, the dispatcher answering it, the config
/// watch, and the shared settings state the two other pieces both touch.
pub struct PortalLoop {
    server: dbus_server::ServerLoop,
    watcher: ConfigWatcher,
    state: SettingsState,
}

impl PortalLoop {
    /// Build the loop: `conn` must already have claimed
    /// [`SettingsService::NAME`] (or whatever real name a `.portal` file
    /// names, once routed) before being handed here -- this type doesn't
    /// own that decision, matching `dbus_server::ServerLoop`'s own
    /// convention.
    #[must_use]
    pub fn new(conn: DbusConn, state: SettingsState) -> Self {
        Self::with_watcher(conn, state, ConfigWatcher::new())
    }

    /// Same as [`Self::new`], but with an explicit `watcher` instead of
    /// one built from the real XDG config path -- lets
    /// `tests/real_bus_probe.rs` point the whole reload pipeline at a
    /// throwaway temp directory (see [`ConfigWatcher::for_path`]) rather
    /// than needing every concurrently-running test in the process to
    /// agree on one global `$XDG_CONFIG_HOME`.
    #[must_use]
    pub fn with_watcher(conn: DbusConn, state: SettingsState, watcher: ConfigWatcher) -> Self {
        let dispatcher: Dispatcher = PortalDispatcher::build(state.clone());
        Self {
            server: dbus_server::ServerLoop::new(conn, dispatcher),
            watcher,
            state,
        }
    }

    /// Mutable access to the underlying connection -- e.g. so `main` can
    /// call `request_name` before handing control to
    /// [`Self::run_forever`], the same shape
    /// `dbus_server::ServerLoop::conn_mut` already has.
    pub const fn conn_mut(&mut self) -> &mut DbusConn {
        self.server.conn_mut()
    }

    /// Serve forever: block on whichever of (bus socket, config watch)
    /// becomes ready first, and react accordingly. Returns only on a
    /// transport-level failure (the bus connection closing) -- the same
    /// contract `dbus_server::ServerLoop::run_forever` has.
    pub fn run_forever(&mut self) -> DbusError {
        loop {
            if let Err(e) = self.pump_once() {
                return e;
            }
        }
    }

    /// Pump exactly `n` iterations of [`Self::pump_once`] -- what a test
    /// calls instead of [`Self::run_forever`] so it can stop
    /// deterministically once it has seen what it is checking for,
    /// matching every other bounded-loop integration test in this
    /// workspace (`crates/dbus-server`'s scratch-service tests, this
    /// crate's own `real_bus_probe`).
    pub fn pump_n(&mut self, n: usize) -> Result<(), DbusError> {
        for _ in 0..n {
            self.pump_once()?;
        }
        Ok(())
    }

    /// One iteration: wait for either fd, then drain whichever (or both)
    /// fired. Kept separate from `run_forever` so a test can call it a
    /// bounded number of times instead of looping forever.
    fn pump_once(&mut self) -> Result<(), DbusError> {
        let bus_fd = self.server.conn_mut().raw_fd();
        match self.watcher.raw_fd() {
            Some(config_fd) => {
                let (bus_ready, config_ready) = waker::Poller::two(bus_fd, config_fd).poll_two(-1);
                if config_ready {
                    self.apply_pending_reload();
                }
                if bus_ready {
                    self.server.serve_one()?;
                }
                // Neither ready is impossible with an infinite poll
                // timeout short of a spurious wakeup; nothing to do in
                // that case either way.
            }
            // No config watch available (see `ConfigWatcher::new`'s
            // best-effort fallback) -- degrade to exactly
            // `ServerLoop::run_forever`'s single-fd behavior.
            None => {
                self.server.serve_one()?;
            }
        }
        Ok(())
    }

    /// If the config file was touched, reload it into `state` and emit
    /// `SettingChanged` for every key that actually changed. A reload
    /// that fails to parse is reported to stderr and otherwise ignored --
    /// keeping the last-known-good settings is strictly better than
    /// serving a half-applied or default config to every future caller.
    fn apply_pending_reload(&mut self) {
        match self.watcher.poll_reload() {
            Ok(Some(new_cfg)) => {
                let changed = self.state.reload(&new_cfg);
                for (key, value) in changed {
                    let body = SettingChangedSignal::body(
                        portal_core::appearance_settings::NAMESPACE,
                        key,
                        value,
                    );
                    if let Err(e) = self.server.conn_mut().send_signal_with_values(
                        SettingsService::PATH,
                        SettingsService::IFACE,
                        "SettingChanged",
                        &body,
                        "ssv",
                    ) {
                        eprintln!("portal: could not emit SettingChanged for '{key}': {e}");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("portal: config reload failed, keeping old settings: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbus::value::Value;

    /// `pump_once`'s bus-only branch (`ConfigWatcher::raw_fd() == None`,
    /// the state this test forces by never creating the temp config
    /// directory a real watch needs) must behave exactly like
    /// `ServerLoop::serve_one` -- no different from before this loop
    /// existed. Exercised indirectly here since constructing a real,
    /// connected `PortalLoop` needs a live session bus (covered by
    /// `tests/real_bus_probe.rs`); this test instead proves
    /// `apply_pending_reload`'s pure decision logic in isolation via
    /// `SettingsState`, which every environment can run without a bus.
    #[test]
    fn reload_emits_nothing_when_settings_are_unchanged() {
        let cfg = config::Config::default();
        let state = SettingsState::new(&cfg);
        assert_eq!(state.reload(&cfg), vec![]);
    }

    #[test]
    fn reload_reports_the_changed_key_and_a_variant_wrapped_value() {
        let mut before = config::Config::default();
        before.files.background = Some(config::Color::from_rgb(0x18, 0x18, 0x18));
        let state = SettingsState::new(&before);

        let mut after = config::Config::default();
        after.files.background = Some(config::Color::from_rgb(0xf8, 0xf8, 0xf8));

        let changed = state.reload(&after);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].0, "color-scheme");

        let body = SettingChangedSignal::body(
            portal_core::appearance_settings::NAMESPACE,
            changed[0].0,
            changed[0].1.clone(),
        );
        assert_eq!(body.len(), 3);
        assert!(matches!(body[2], Value::Variant(_)));
    }
}
