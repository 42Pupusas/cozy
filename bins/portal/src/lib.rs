//! Library half of the `portal` binary, existing so
//! `tests/real_bus_probe.rs` (a `dbus-server`-style real-session-bus
//! integration test, per `docs/portal-scoping.md` step 5's "validates the
//! real frontend path" goal) can construct [`SettingsService::dispatcher`]
//! and [`PortalLoop`] without linking `main`'s process-exit control flow.

pub mod config_watcher;
pub mod email_service;
pub mod mail_launcher;
pub mod portal_dispatcher;
pub mod portal_loop;
pub mod settings_service;
pub mod settings_state;

pub use config_watcher::ConfigWatcher;
pub use email_service::EmailService;
pub use mail_launcher::{AssociatedMailClient, MailLauncher};
pub use portal_dispatcher::PortalDispatcher;
pub use portal_loop::PortalLoop;
pub use settings_service::SettingsService;
pub use settings_state::SettingsState;
