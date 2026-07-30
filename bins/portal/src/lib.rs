//! Library half of the `portal` binary, existing solely so
//! `tests/real_bus_probe.rs` (a `dbus-server`-style real-session-bus
//! integration test, per `docs/portal-scoping.md` step 5's "validates the
//! real frontend path" goal) can construct [`SettingsService::dispatcher`]
//! without linking `main`'s process-exit control flow.

pub mod settings_service;

pub use settings_service::SettingsService;
