//! Serves the portal's `Dispatcher` while also watching the config file
//! for saves, running file chooser dialogs, and noticing callers that leave
//! the bus.
//!
//! `dbus_server::ServerLoop::run_forever` alone cannot do this: it blocks
//! forever inside one blocking `io_uring` recv on the bus socket. This is
//! the portal-specific event pump -- poll the bus socket, the config watch
//! and every open dialog's wake fd with one `poll(2)` (`waker::PollSet`),
//! and only then act on whichever source is ready.

use std::sync::Arc;

use dbus::NameOwnerChanged;
use dbus::conn::{DbusConn, DbusError};
use dbus_server::Received;
use portal_core::{DialogFactory, SettingChangedSignal, UnavailableDialogs};
use waker::{Interest, PollSet};

use crate::chooser_engine::ChooserEngine;
use crate::chooser_inbox::ChooserInbox;
use crate::config_watcher::ConfigWatcher;
use crate::mail_launcher::{AssociatedMailClient, MailLauncher};
use crate::portal_dispatcher::PortalDispatcher;
use crate::settings_service::SettingsService;
use crate::settings_state::SettingsState;

/// Which sources had work when the last wait ended.
struct Readiness {
    bus: bool,
    config: bool,
}

/// Owns the live connection, the dispatcher answering it, the config
/// watch, the shared settings state, and the open file chooser dialogs.
pub struct PortalLoop {
    server: dbus_server::ServerLoop,
    watcher: ConfigWatcher,
    state: SettingsState,
    chooser: ChooserEngine,
}

impl PortalLoop {
    const NAME_OWNER_RULE: &'static str = "type='signal',sender='org.freedesktop.DBus',\
         interface='org.freedesktop.DBus',member='NameOwnerChanged'";

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
        Self::with_dialogs(conn, state, watcher, Box::new(UnavailableDialogs))
    }

    /// Same as [`Self::with_watcher`], with the factory that opens file
    /// chooser dialogs.
    #[must_use]
    pub fn with_dialogs(
        conn: DbusConn,
        state: SettingsState,
        watcher: ConfigWatcher,
        dialogs: Box<dyn DialogFactory>,
    ) -> Self {
        Self::assemble(
            conn,
            state,
            watcher,
            dialogs,
            Arc::new(AssociatedMailClient),
        )
    }

    /// Every collaborator explicit, for tests that substitute the mail
    /// client as well.
    #[must_use]
    pub fn assemble(
        conn: DbusConn,
        state: SettingsState,
        watcher: ConfigWatcher,
        dialogs: Box<dyn DialogFactory>,
        mail: Arc<dyn MailLauncher>,
    ) -> Self {
        let inbox = ChooserInbox::new();
        let dispatcher = PortalDispatcher::assemble(state.clone(), mail, &inbox);
        Self {
            server: dbus_server::ServerLoop::new(conn, dispatcher),
            watcher,
            state,
            chooser: ChooserEngine::new(inbox, dialogs),
        }
    }

    /// Mutable access to the underlying connection -- e.g. so `main` can
    /// call `request_name` before handing control to
    /// [`Self::run_forever`], the same shape
    /// `dbus_server::ServerLoop::conn_mut` already has.
    pub const fn conn_mut(&mut self) -> &mut DbusConn {
        self.server.conn_mut()
    }

    /// Subscribe to the bus's announcements of connections going away, so a
    /// dialog is dismissed when the application that asked for it exits.
    ///
    /// # Errors
    ///
    /// Fails if the bus refuses the match rule.
    pub fn watch_caller_exits(&mut self) -> Result<(), DbusError> {
        self.server.conn_mut().add_match(Self::NAME_OWNER_RULE)
    }

    /// How many file chooser dialogs are open.
    #[must_use]
    pub fn open_dialogs(&self) -> usize {
        self.chooser.open_count()
    }

    /// Serve forever: block on whichever of (bus socket, config watch,
    /// dialogs) becomes ready first, and react accordingly. Returns only on
    /// a transport-level failure (the bus connection closing) -- the same
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

    /// One iteration: wait for any source, then drain whichever fired.
    fn pump_once(&mut self) -> Result<(), DbusError> {
        self.serve_queued_messages()?;
        let ready = self.wait_for_work();
        if ready.config {
            self.apply_pending_reload();
        }
        if ready.bus {
            self.serve_bus_message()?;
        }
        self.run_dialogs();
        Ok(())
    }

    /// Messages a nested `call` read ahead are no longer readable on the
    /// socket, so they are served before waiting rather than counted as an
    /// iteration of their own.
    fn serve_queued_messages(&mut self) -> Result<(), DbusError> {
        while self.server.conn_mut().has_pending() {
            self.serve_bus_message()?;
        }
        Ok(())
    }

    fn wait_for_work(&mut self) -> Readiness {
        let mut set = PollSet::new();
        let bus_index = set.watch(self.server.conn_mut().raw_fd(), Interest::Readable);
        let config_index = self
            .watcher
            .raw_fd()
            .map(|fd| set.watch(fd, Interest::Readable));
        for fd in self.chooser.wake_fds() {
            set.watch(fd, Interest::Readable);
        }
        let _ = set.wait(self.chooser.wait_ms().unwrap_or(-1));
        Readiness {
            bus: set.is_ready(bus_index),
            config: config_index.is_some_and(|index| set.is_ready(index)),
        }
    }

    fn serve_bus_message(&mut self) -> Result<(), DbusError> {
        if let Received::Signal(signal) = self.server.serve_message()?
            && signal.member.as_deref() == Some("NameOwnerChanged")
            && let Some(changed) = NameOwnerChanged::from_body(&signal.values())
            && let Some(owner) = changed.lost_owner()
        {
            let (_, dispatcher) = self.server.parts_mut();
            self.chooser.owner_lost(owner, dispatcher);
        }
        Ok(())
    }

    fn run_dialogs(&mut self) {
        let (conn, dispatcher) = self.server.parts_mut();
        self.chooser.pump(conn, dispatcher);
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

    /// Constructing a real, connected `PortalLoop` needs a live session bus
    /// (covered by `tests/real_bus_probe.rs`); this test instead proves
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
